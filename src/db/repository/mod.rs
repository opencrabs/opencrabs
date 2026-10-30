//! Repository Module
//!
//! Repository pattern implementations for database access. One module per
//! table; [`traits`] holds the generic CRUD contract. This file is
//! declarations only — no function definitions live here (CONTRIBUTING.md).

pub mod analytics_event;
pub mod background_task;
pub mod channel_message;
pub mod cron_job;
pub mod cron_job_run;
pub mod decision_cache;
pub mod decision_stats;
pub mod discord_tool_card;
pub mod feedback_ledger;
pub mod file;
pub mod message;
pub mod pending_followup;
pub mod pending_request;
pub mod plan_card;
pub mod project;
pub mod recent_paths;
pub mod session;
pub mod session_binding;
pub mod session_skills;
pub mod tool_execution;
mod traits;
pub mod turn_retrieval;
pub mod usage_ledger;

pub use analytics_event::AnalyticsEventRepository;
pub use background_task::{BackgroundTaskRepository, BackgroundTaskRow};
pub use channel_message::{ChannelMessageRepository, TopicSummary};
pub use cron_job::{CronJobPatch, CronJobRepository};
pub use cron_job_run::CronJobRunRepository;
pub use decision_cache::{DecisionCacheRepository, DecisionCacheRow, DecisionPut};
pub use decision_stats::{DecisionStats, DecisionStatsRepository};
pub use discord_tool_card::DiscordToolCardRepository;
pub use feedback_ledger::FeedbackLedgerRepository;
pub use file::FileRepository;
pub use message::MessageRepository;
pub use pending_followup::{FollowupHost, PendingFollowup, PendingFollowupRepository};
pub use pending_request::PendingRequestRepository;
pub use plan_card::{PlanCard, PlanCardRepository};
pub use project::ProjectRepository;
pub use recent_paths::RecentPathsRepository;
pub use session::{SessionListOptions, SessionRepository};
pub use session_binding::SessionBindingRepository;
pub use session_skills::SessionSkillsRepository;
pub use tool_execution::ToolExecutionRepository;
pub use traits::Repository;
pub use turn_retrieval::{AuditRow, TurnRetrievalRepository};
pub use usage_ledger::UsageLedgerRepository;
pub mod notify_queue;
pub use notify_queue::{NotifyQueueRepository, NotifyQueueRow};
