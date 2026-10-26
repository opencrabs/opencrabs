//! Agent Service Implementation
//!
//! Core service for managing AI agent conversations, coordinating between
//! LLM providers, context management, and data persistence.

pub(crate) mod announcement_loop;
pub(crate) mod background_tasks;
pub(crate) mod boot_report;
pub(crate) mod builder;
pub(crate) mod clear;
pub(crate) mod compaction;
pub(crate) mod compaction_notice;
pub(crate) mod compaction_prompts;
pub(crate) mod compaction_stream;
pub(crate) mod context;
pub(crate) mod context_rows;
pub(crate) mod work_status;
#[allow(unused_imports)] // only used in test code
pub(crate) use context::{format_editing_reminder, format_plan_reminder, plan_state_block};
pub(crate) mod failure_window;
pub(crate) mod fallback_suggest;
pub(crate) mod feedback;
pub(crate) mod fenced_command;
mod gaslighting;
pub(crate) mod halt_answer;
pub(crate) mod helpers;
pub(crate) mod loop_break;
mod messaging;
mod model_refresh;
pub(crate) mod notify_policy;
pub(crate) mod notify_queue;
pub(crate) mod notify_receipts;
pub(crate) mod nudge;
pub(crate) mod parallel_tools;
pub(crate) mod phantom;
pub(crate) mod phantom_lang;
pub(crate) mod plan_mode_provider;
pub(crate) mod queued_fold;
pub(crate) mod quiet_delivery;
pub(crate) mod reasoning_run;
pub(crate) mod repetition;
pub(crate) mod request_budget;
pub(crate) mod restart_recovery;
pub(crate) mod session_cwd;
pub(crate) mod session_routes;
pub(crate) mod shutdown;
pub(crate) mod tool_loop;
pub(crate) mod tool_repeat;
pub(crate) mod truncation;
mod types;

pub use builder::{AgentService, BrainRebuild};
pub use gaslighting::{is_gaslighting_preamble, strip_gaslighting_preamble};
pub use helpers::{detect_text_repetition, provider_matches_session};
pub use model_refresh::should_refresh_session_model;
pub use phantom::{
    claims_unbacked_side_effects, count_intent_line_starts, count_unbacked_side_effect_claims,
    has_forward_intent_post_success, has_investigative_intent, has_phantom_tool_intent,
    has_phantom_tool_intent_no_tools, is_analysis_intent, is_bare_completion_only,
    is_delivery_intent, is_stuck_in_intent_loop, looks_truncated_mid_sentence,
};
pub use types::{
    AgentResponse, AgentStreamResponse, ApprovalCallback, BgTaskMeta, ChannelSessionEvent,
    MessageEnqueueCallback, MessageQueueCallback, PendingOrigin, ProgressCallback, ProgressEvent,
    PushOrigin, QueuedUserMessage, SshPasswordCallback, SudoCallback, ToolApprovalInfo,
};
