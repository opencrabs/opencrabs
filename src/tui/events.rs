//! TUI Event System
//!
//! Handles user input and application events for the terminal interface.

use crate::brain::agent::AgentResponse;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Events that can occur in the TUI
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// User pressed a key
    Key(KeyEvent),

    /// Mouse scroll event
    MouseScroll(i8), // positive = up, negative = down

    /// Mouse left-click at (column, row) — select message
    MouseClick(u16, u16),

    /// Mouse right-click at (column, row) — copy message
    MouseRightClick(u16, u16),

    /// Mouse left-button drag to (column, row) — extend text selection
    MouseDrag(u16, u16),

    /// Mouse left-button released at (column, row) — finalize selection + copy
    MouseUp(u16, u16),

    /// Terminal gained focus
    FocusGained,

    /// Terminal lost focus
    FocusLost,

    /// User pasted text
    Paste(String),

    /// Terminal was resized
    Resize(u16, u16),

    /// User submitted a message
    MessageSubmitted(String),

    /// A slash command, skill, or user-command expansion submitted a prompt.
    /// Same send path as `MessageSubmitted` but the prompt analyzer is
    /// skipped: soft-nudge is for natural-language user chat only.
    CommandSubmitted(String),

    /// Agent started processing
    AgentProcessing,

    /// Agent sent a response chunk (streaming)
    ResponseChunk { session_id: Uuid, text: String },

    /// Wipe the in-progress streaming buffer for a session — used when a
    /// gaslighting refusal preamble has been detected mid-turn and must
    /// not stay on screen while the tools it contradicted actually execute.
    StripStreamedContent {
        session_id: Uuid,
        /// Bytes to strip from the start of the streaming response buffer.
        bytes: usize,
        reason: String,
    },

    /// Agent completed response
    ResponseComplete {
        session_id: Uuid,
        response: AgentResponse,
    },

    /// An error occurred
    Error { session_id: Uuid, message: String },

    /// Request to switch UI mode
    SwitchMode(AppMode),

    /// Request to select a session
    SelectSession(Uuid),

    /// Request to create new session
    NewSession,

    /// Request to quit
    Quit,

    /// Tick event for animations/updates
    Tick,

    /// Tool approval requested
    ToolApprovalRequested(ToolApprovalRequest),

    /// Tool approval response
    ToolApprovalResponse(ToolApprovalResponse),

    /// A tool call has started executing
    ToolCallStarted {
        session_id: Uuid,
        tool_name: String,
        tool_input: Value,
    },

    /// A tool call has completed
    ToolCallCompleted {
        session_id: Uuid,
        tool_name: String,
        tool_input: Value,
        success: bool,
        summary: String,
    },

    /// Intermediate text the agent sent between tool call batches
    IntermediateText {
        session_id: Uuid,
        text: String,
        reasoning: Option<String>,
    },

    /// Context was auto-compacted — show the summary to the user
    CompactionSummary { session_id: Uuid, summary: String },

    /// A single build-output line — TUI keeps a rolling window
    BuildLine(String),

    /// Build completed — restart into the new binary. `binary_path` is the
    /// exact binary to exec when known (a source build); `None` means resolve via
    /// `current_exe()` (`/evolve` replaced the running exe in place). Global,
    /// not per-session.
    RestartReady {
        status: String,
        binary_path: Option<std::path::PathBuf>,
    },

    /// Configuration was reloaded (e.g. after config_tool write)
    ConfigReloaded,

    /// Real-time token count update from the agent loop
    TokenCountUpdated { session_id: Uuid, count: usize },

    /// Streaming output token count (per-response, counted via tiktoken)
    StreamingOutputTokens { session_id: Uuid, tokens: u32 },

    /// Onboarding wizard received fetched model list from provider API
    OnboardingModelsFetched(Vec<String>),

    /// Model selector (/models) received fetched model list (provider_index, models)

    /// WhatsApp QR code data received during onboarding pairing
    WhatsAppQrCode(String),
    /// WhatsApp pairing successful during onboarding
    WhatsAppConnected,
    /// WhatsApp pairing failed during onboarding
    WhatsAppError(String),

    /// GitHub Copilot device flow: display this code to the user
    GitHubDeviceCode(String),
    /// GitHub Copilot device flow: OAuth token obtained
    GitHubOAuthComplete(String),
    /// GitHub Copilot device flow: failed
    GitHubOAuthError(String),

    /// Codex OAuth device flow: display this code to the user
    CodexDeviceCode(String),
    /// Codex OAuth device flow: authentication complete
    CodexOAuthComplete,
    /// Codex OAuth device flow: failed
    CodexOAuthError(String),

    /// A system message to display in chat. Carries the originating session
    /// id so the TUI only renders the message inside that session and other
    /// open panes don't see leaked self-healing alerts from sessions they
    /// aren't currently focused on.
    SystemMessage { session_id: Uuid, text: String },

    /// Sticky fallback just swapped the active provider/model. Carries the
    /// originating session id so a fallback in session A doesn't update
    /// the footer or current session record while the user is looking at
    /// session B; the swap is always persisted to the originating session's
    /// DB record so it shows correctly when the user navigates back.
    ProviderSwitched {
        session_id: Uuid,
        to_name: String,
        to_model: String,
        reason: String,
    },

    /// Optional follow-up suggestions surfaced by the `suggest_options` tool.
    /// Carries the originating session id so suggestions only appear in that
    /// session's input. One option renders as ghost text in the input (Tab
    /// fills it); several render as a pick-list. Accepting fills the input as
    /// editable text and never submits.
    SuggestedOptions {
        session_id: Uuid,
        options: Vec<String>,
    },

    /// Update available — show prompt dialog with version string
    UpdateAvailable(String),

    /// Channel test message result during onboarding
    ChannelTestResult {
        channel: String,
        success: bool,
        error: Option<String>,
        /// Auto-detected Telegram user ID (from getUpdates) — None for non-Telegram channels
        detected_telegram_user_id: Option<String>,
    },

    /// Brain generation completed during onboarding (last step).
    /// Either Ok with the LLM response text or Err with the failure reason.
    BrainGenerationResult { result: Result<String, String> },

    /// Sudo password requested by bash tool
    SudoPasswordRequested(SudoPasswordRequest),

    /// SSH password requested by bash tool (used when key auth fails on
    /// `ssh`/`scp`/`rsync`). Mirrors `SudoPasswordRequested` so the dialog
    /// plumbing is identical — only the title and prompt label differ.
    SshPasswordRequested(SshPasswordRequest),

    /// Reasoning/thinking content chunk from providers like MiniMax (display-only)
    ReasoningChunk { session_id: Uuid, text: String },

    /// A queued user message was injected into the tool loop between iterations
    QueuedUserMessage { session_id: Uuid, text: String },

    /// Whisper model download progress (0.0–1.0)
    WhisperDownloadProgress(f64),
    /// Whisper model download completed (Ok or Err message)
    WhisperDownloadComplete(Result<(), String>),

    /// Piper voice download progress (0.0–1.0)
    PiperDownloadProgress(f64),
    /// Piper voice download completed (Ok(voice_id) or Err message)
    PiperDownloadComplete(Result<String, String>),

    /// A remote channel (Telegram, WhatsApp, Discord, Slack) completed an agent
    /// response — the TUI should refresh if it's the current session.
    SessionUpdated(Uuid),

    /// Session title was generated/updated (e.g. by the auto-title path in
    /// `tool_loop`). Cheap in-memory refresh of `current_session.title` and
    /// the cached sessions list — no DB reload, no message reshuffle. Without
    /// this, the footer kept showing "New Chat" after Ctrl+N until the user
    /// switched sessions, because the auto-title spawn wrote to DB but never
    /// notified the TUI. The full `SessionUpdated` event triggers a heavy
    /// `load_session` which is overkill for a title change.
    SessionTitleUpdated { session_id: Uuid, title: String },

    /// A remote channel started processing a session — TUI should block sends
    /// on this session to prevent concurrent tool loops.
    ChannelProcessingStarted(Uuid),
    /// A remote channel finished processing a session.
    ChannelProcessingFinished(Uuid),

    /// A pending request was resumed on startup — TUI must track the cancel token
    /// so double-Escape can abort it.
    PendingResumed {
        session_id: Uuid,
        cancel_token: CancellationToken,
    },

    /// A background task (#722) finished — resume `session_id`: inject mid-turn
    /// if it's still processing, else start a fresh turn. `context_text` is what
    /// the agent sees; `display_text` is the compact tag for history.
    BackgroundTaskDone {
        session_id: Uuid,
        context_text: String,
        display_text: String,
    },

    /// A detached task started while its turn was still running (#2146).
    ///
    /// Carries the id the finish repeats, so the TUI draws ONE row for the task
    /// inside the turn's tool block and updates that row when the finish lands
    /// instead of opening a second block.
    DetachedTaskStarted {
        session_id: Uuid,
        task_id: String,
        label: String,
    },

    /// A detached task finished (#2146). Same `task_id` as the start, so the
    /// row is found by identity rather than by label. A finish with no row
    /// (the turn's block is gone, or the task outlived a restart) is not an
    /// error. Since #2218 the receipt push is the fallback for a task with no
    /// progress callback, so a surface that received this event must not
    /// expect `BackgroundTaskDone` to follow it.
    DetachedTaskFinished {
        session_id: Uuid,
        task_id: String,
        label: String,
        success: bool,
        elapsed_secs: f32,
    },

    /// A detached launcher exited non-zero after its `spawn` already
    /// succeeded (#2170, #2172). `spawn` only proves the binary existed, so
    /// the optimistic "Opened …" line is corrected when this lands.
    LauncherFailed { target: String, detail: String },
}

/// Sudo password request from the bash tool
#[derive(Debug)]
pub struct SudoPasswordRequest {
    /// Unique ID for this request
    pub request_id: Uuid,
    /// The sudo command being run
    pub command: String,
    /// Channel to send password back
    pub response_tx: mpsc::UnboundedSender<SudoPasswordResponse>,
}

// Manual Clone — response_tx is Clone-able (UnboundedSender)
impl Clone for SudoPasswordRequest {
    fn clone(&self) -> Self {
        Self {
            request_id: self.request_id,
            command: self.command.clone(),
            response_tx: self.response_tx.clone(),
        }
    }
}

/// Sudo password response from the TUI
#[derive(Debug, Clone)]
pub struct SudoPasswordResponse {
    /// The password (None if cancelled by user)
    pub password: Option<String>,
}

/// SSH password request from the bash tool. Same shape as
/// `SudoPasswordRequest` — `command` carries a human-friendly label
/// (e.g. `"root@1.2.3.4 (ssh)"`) instead of the literal sudo command,
/// since the bash tool may have already rewritten the SSH invocation.
#[derive(Debug)]
pub struct SshPasswordRequest {
    pub request_id: Uuid,
    /// Target description shown to the user (e.g. `"root@1.2.3.4 (ssh)"`).
    pub target: String,
    /// Channel to send password back
    pub response_tx: mpsc::UnboundedSender<SshPasswordResponse>,
}

impl Clone for SshPasswordRequest {
    fn clone(&self) -> Self {
        Self {
            request_id: self.request_id,
            target: self.target.clone(),
            response_tx: self.response_tx.clone(),
        }
    }
}

/// SSH password response from the TUI
#[derive(Debug, Clone)]
pub struct SshPasswordResponse {
    pub password: Option<String>,
}

/// Tool approval request details
#[derive(Debug, Clone)]
pub struct ToolApprovalRequest {
    /// Unique ID for this approval request
    pub request_id: Uuid,

    /// Session this approval belongs to
    pub session_id: Uuid,

    /// Tool name
    pub tool_name: String,

    /// Tool description
    pub tool_description: String,

    /// Tool input parameters
    pub tool_input: Value,

    /// Tool capabilities
    pub capabilities: Vec<String>,

    /// Channel to send response back
    pub response_tx: mpsc::UnboundedSender<ToolApprovalResponse>,

    /// When this request was created (for timeout)
    pub requested_at: std::time::Instant,
}

impl ToolApprovalRequest {
    /// How long this request has been waiting
    pub fn elapsed(&self) -> std::time::Duration {
        self.requested_at.elapsed()
    }
}

/// Tool approval response
#[derive(Debug, Clone)]
pub struct ToolApprovalResponse {
    /// Request ID this is responding to
    pub request_id: Uuid,

    /// Whether the user approved
    pub approved: bool,

    /// Optional reason for denial
    pub reason: Option<String>,
}

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Main chat interface (full execution)
    Chat,
    /// Session list/management
    Sessions,
    /// Help screen
    Help,
    /// Plan overlay: scrollable design .md with Approve/Discard footer
    PlanOverlay,
    /// Settings
    Settings,
    /// File picker dialog (triggered by @)
    FilePicker,
    /// Model selector dialog (triggered by /models)
    /// Full-screen usage dashboard (triggered by /usage)
    UsageDashboard,
    /// Restart confirmation pending (after a successful update)
    RestartPending,
    /// Update prompt — ask user to accept or decline update
    UpdatePrompt,
    /// Directory picker dialog (triggered by /cd)
    DirectoryPicker,
    /// Onboarding wizard
    Onboarding,
    /// Mission Control — RSI proposals, activity, schedule (triggered by /mission-control)
    MissionControl,
    /// Skills picker — filterable list of loaded skills (triggered by /skills)
    SkillsList,
    /// Session files — browse files tracked for a session (triggered by F in /sessions)
    SessionFiles,
    /// Projects — manage projects and assign sessions (triggered by P in /sessions)
    Projects,
    /// Profiles — manage OpenCrabs profiles (triggered by /profiles)
    Profiles,
}

/// Event handler for the TUI
pub struct EventHandler {
    /// Event sender
    tx: mpsc::UnboundedSender<TuiEvent>,

    /// Event receiver
    rx: mpsc::UnboundedReceiver<TuiEvent>,
}

impl EventHandler {
    /// Create a new event handler
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx }
    }

    /// Get a sender for sending events
    pub fn sender(&self) -> mpsc::UnboundedSender<TuiEvent> {
        self.tx.clone()
    }

    /// Receive the next event (blocks until available)
    pub async fn next(&mut self) -> Option<TuiEvent> {
        self.rx.recv().await
    }

    /// Try to receive the next event without blocking
    pub fn try_next(&mut self) -> Option<TuiEvent> {
        self.rx.try_recv().ok()
    }

    /// Start listening for terminal events
    ///
    /// Uses crossterm's async EventStream instead of blocking poll/read
    /// to avoid starving the tokio runtime during I/O-heavy operations
    /// (e.g. Telegram voice processing, agent responses).
    ///
    /// Returns the task's `JoinHandle` so the owner can abort it. The editor
    /// handoff (#1744) MUST stop this reader before suspending the TUI, or
    /// it races the child process for the same stdin fd.
    pub fn start_terminal_listener(
        tx: mpsc::UnboundedSender<TuiEvent>,
    ) -> tokio::task::JoinHandle<()> {
        use crossterm::event::EventStream;
        use futures::StreamExt;

        tokio::spawn(async move {
            let mut reader = EventStream::new();
            let tick_interval = std::time::Duration::from_millis(100);

            loop {
                // Race: next terminal event vs tick timer
                let event = tokio::select! {
                    maybe_event = reader.next() => {
                        match maybe_event {
                            Some(Ok(event)) => Some(event),
                            Some(Err(_)) => None,
                            None => break, // Stream closed
                        }
                    }
                    _ = tokio::time::sleep(tick_interval) => None,
                };

                if let Some(event) = event {
                    let should_break = match event {
                        crossterm::event::Event::Key(key) => {
                            // Only process key press events to avoid duplicates
                            if key.kind == crossterm::event::KeyEventKind::Press {
                                tx.send(TuiEvent::Key(key)).is_err()
                            } else {
                                false
                            }
                        }
                        crossterm::event::Event::Mouse(mouse) => {
                            use crossterm::event::MouseEventKind;
                            match mouse.kind {
                                MouseEventKind::ScrollUp => {
                                    tx.send(TuiEvent::MouseScroll(1)).is_err()
                                }
                                MouseEventKind::ScrollDown => {
                                    tx.send(TuiEvent::MouseScroll(-1)).is_err()
                                }
                                MouseEventKind::Down(crossterm::event::MouseButton::Left) => tx
                                    .send(TuiEvent::MouseClick(mouse.column, mouse.row))
                                    .is_err(),
                                MouseEventKind::Down(crossterm::event::MouseButton::Right) => tx
                                    .send(TuiEvent::MouseRightClick(mouse.column, mouse.row))
                                    .is_err(),
                                MouseEventKind::Drag(crossterm::event::MouseButton::Left) => tx
                                    .send(TuiEvent::MouseDrag(mouse.column, mouse.row))
                                    .is_err(),
                                MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                                    tx.send(TuiEvent::MouseUp(mouse.column, mouse.row)).is_err()
                                }
                                _ => false,
                            }
                        }
                        crossterm::event::Event::Resize(w, h) => {
                            tx.send(TuiEvent::Resize(w, h)).is_err()
                        }
                        crossterm::event::Event::Paste(text) => {
                            tx.send(TuiEvent::Paste(text)).is_err()
                        }
                        crossterm::event::Event::FocusGained => {
                            tx.send(TuiEvent::FocusGained).is_err()
                        }
                        crossterm::event::Event::FocusLost => tx.send(TuiEvent::FocusLost).is_err(),
                    };
                    if should_break {
                        break;
                    }
                }

                // Send tick event for animations
                if tx.send(TuiEvent::Tick).is_err() {
                    break;
                }
            }
        })
    }
}

impl Default for EventHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to check if a key event matches
pub fn key_matches(event: &KeyEvent, code: KeyCode, modifiers: KeyModifiers) -> bool {
    event.code == code && event.modifiers == modifiers
}

/// Common key bindings
pub mod keys {
    use super::*;

    /// Ctrl+V or Cmd+V: paste from the OS clipboard. Kitty-protocol
    /// terminals that pass Cmd+V through report it as SUPER (#1812).
    pub fn is_clipboard_paste(event: &KeyEvent) -> bool {
        matches!(event.code, KeyCode::Char('v') | KeyCode::Char('V'))
            && (event.modifiers == KeyModifiers::CONTROL || event.modifiers == KeyModifiers::SUPER)
    }

    /// Whether a character key held with `modifiers` types its character.
    /// Ctrl chords are commands except AltGr (reported as Ctrl+Alt), which
    /// composes characters. Cmd (SUPER) chords never type text (#1812).
    pub fn types_character(modifiers: KeyModifiers) -> bool {
        !modifiers.contains(KeyModifiers::SUPER)
            && (!modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT))
    }

    /// Ctrl+C - Quit
    pub fn is_quit(event: &KeyEvent) -> bool {
        key_matches(event, KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    /// Ctrl+N - New session
    pub fn is_new_session(event: &KeyEvent) -> bool {
        key_matches(event, KeyCode::Char('n'), KeyModifiers::CONTROL)
    }

    /// Ctrl+L - List sessions
    pub fn is_list_sessions(event: &KeyEvent) -> bool {
        key_matches(event, KeyCode::Char('l'), KeyModifiers::CONTROL)
    }

    /// Ctrl+K - Clear current session
    pub fn is_clear_session(event: &KeyEvent) -> bool {
        key_matches(event, KeyCode::Char('k'), KeyModifiers::CONTROL)
    }

    /// Enter - Submit (plain Enter sends the message)
    /// Also accepts Ctrl+Enter for backwards compatibility
    pub fn is_submit(event: &KeyEvent) -> bool {
        event.code == KeyCode::Enter
            && (event.modifiers.is_empty() || event.modifiers.contains(KeyModifiers::CONTROL))
    }

    /// Insert newline — Alt+Enter, Shift+Enter, or Ctrl+J
    /// macOS terminals don't send ALT modifier for Option key, so Ctrl+J
    /// (Unix standard line feed) is the reliable cross-platform binding.
    pub fn is_newline(event: &KeyEvent) -> bool {
        (event.code == KeyCode::Enter
            && (event.modifiers.contains(KeyModifiers::ALT)
                || event.modifiers.contains(KeyModifiers::SHIFT)))
            || (event.code == KeyCode::Char('j') && event.modifiers.contains(KeyModifiers::CONTROL))
    }

    /// Escape - Cancel/Back
    pub fn is_cancel(event: &KeyEvent) -> bool {
        event.code == KeyCode::Esc
    }

    /// Enter - Select/Confirm
    pub fn is_enter(event: &KeyEvent) -> bool {
        event.code == KeyCode::Enter && event.modifiers.is_empty()
    }

    /// Up arrow
    pub fn is_up(event: &KeyEvent) -> bool {
        event.code == KeyCode::Up && event.modifiers.is_empty()
    }

    /// Down arrow
    pub fn is_down(event: &KeyEvent) -> bool {
        event.code == KeyCode::Down && event.modifiers.is_empty()
    }

    /// Left arrow
    pub fn is_left(event: &KeyEvent) -> bool {
        event.code == KeyCode::Left && event.modifiers.is_empty()
    }

    /// Right arrow
    pub fn is_right(event: &KeyEvent) -> bool {
        event.code == KeyCode::Right && event.modifiers.is_empty()
    }

    /// Page up
    pub fn is_page_up(event: &KeyEvent) -> bool {
        event.code == KeyCode::PageUp
    }

    /// Page down
    pub fn is_page_down(event: &KeyEvent) -> bool {
        event.code == KeyCode::PageDown
    }

    /// Tab - Select/Navigate
    pub fn is_tab(event: &KeyEvent) -> bool {
        event.code == KeyCode::Tab && event.modifiers.is_empty()
    }

    /// 'A' or 'Y' - Approve
    pub fn is_approve(event: &KeyEvent) -> bool {
        matches!(
            event.code,
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('y') | KeyCode::Char('Y')
        ) && event.modifiers.is_empty()
    }

    /// 'D' or 'N' - Deny
    pub fn is_deny(event: &KeyEvent) -> bool {
        matches!(
            event.code,
            KeyCode::Char('d')
                | KeyCode::Char('D')
                | KeyCode::Char('n')
                | KeyCode::Char('N')
                // `r` — reject synonym the shared footer advertises
                // (#1775); same semantics as D, no modifier combos.
                | KeyCode::Char('r')
        ) && event.modifiers.is_empty()
    }

    /// 'V' - View details
    pub fn is_view_details(event: &KeyEvent) -> bool {
        matches!(event.code, KeyCode::Char('v') | KeyCode::Char('V')) && event.modifiers.is_empty()
    }

    /// Ctrl+X — close focused pane
    pub fn is_close_pane(event: &KeyEvent) -> bool {
        key_matches(event, KeyCode::Char('x'), KeyModifiers::CONTROL)
    }

    /// Tab — cycle focus to next pane (only when split mode is active)
    pub fn is_focus_next_pane(event: &KeyEvent) -> bool {
        event.code == KeyCode::Tab && event.modifiers.is_empty()
    }
}
