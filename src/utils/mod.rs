//! Utility modules for common functionality

pub mod approval;
pub mod command_label;
pub(crate) mod config_reload_reason;
pub mod config_watcher;
pub mod cwd;
pub mod directives;
pub mod drop_agent;
pub mod drop_landing;
pub mod drop_transfer;
pub mod echo_budget;
pub mod fd_suppress;
pub mod file_extract;
pub mod git_branch;
pub mod image;
pub mod install;
pub mod long_command;
pub mod model_match;
pub mod pdf_vision;
pub mod plan_files;
pub mod plan_mode;
pub mod prompt_analyzer;
pub mod provider_pair;
pub mod providers;
pub mod retry;
pub mod sanitize;
pub mod shell;
pub mod shell_scan;
pub mod slack_fmt;
pub mod stop_intent;
pub mod string;
pub mod text_complete;
mod tool_context;
pub mod tree_view;

pub use approval::{
    check_approval_policy, persist_auto_always_policy, persist_auto_session_policy,
};
pub use directives::extract_leaked_suggestions;
pub use file_extract::{FileContent, classify_file, inject_file_content, process_file_with_vision};
pub use image::{
    DOC_ID_PREFIX, LocalFile, LocalFileRewrite, LocalFileScan, LocalImageFailure,
    LocalImageFailureReason, ResolvedFileRef, TELEGRAM_DOCUMENT_MAX_BYTES,
    append_file_failure_notice, code_regions, extract_img_markers, extract_local_files,
    extract_react_marker, extract_react_marker_lenient, extract_vid_markers, file_failure_notice,
    rewrite_local_files, strip_invalid_react_markers, validate_local_file,
};
pub use prompt_analyzer::PromptAnalyzer;
pub use retry::{RetryConfig, RetryableError, retry, retry_with_check};
pub use sanitize::{redact_secrets, redact_secrets_scoped, redact_tool_input};
pub use string::{
    format_ctx_footer, prefix_before_last_char, strip_ctx_footer, truncate_chars, truncate_str,
    truncate_str_tail,
};
pub use tool_context::{tool_context_hint, tool_status_source};
