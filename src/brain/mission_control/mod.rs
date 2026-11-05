//! Mission Control — data services
//!
//! Aggregates and adapts data from existing OpenCrabs subsystems
//! (`rsi_proposals`, RSI improvements log, cron registry, approval queue)
//! into uniform per-panel types the TUI renderer consumes.
//!
//! Each service is a small, stateless function that pulls fresh data on
//! demand. There's deliberately no caching here — Mission Control is
//! a read-mostly UI that re-fetches on draw. If panels grow expensive
//! we can add a cheap snapshot layer behind the same trait.
//!
//! Layout:
//!
//! ```text
//! src/brain/mission_control/
//! ├── mod.rs                # public API + re-exports
//! ├── types.rs              # McInboxItem, McActivity, McScheduleItem
//! ├── inbox_service.rs      # RSI proposals → Vec<McInboxItem>     (C9)
//! ├── activity_service.rs   # improvements log + ledger → Vec<McActivity> (C10)
//! └── schedule_service.rs   # cron + pending approvals → Vec<McScheduleItem> (C10)
//! ```
//!
//! The skeleton lands in C8; services land in C9/C10.

pub mod activity_service;
pub mod analytics_service;
pub mod inbox_service;
pub mod schedule_service;
pub mod staleness;
pub mod types;

/// Maximum activity entries loaded by every Mission Control frontend.
/// Keeping the fetch limit here prevents the terminal and GUI snapshots
/// from silently covering different windows.
pub const ACTIVITY_LIMIT: usize = 100;

pub use types::{
    McActivity, McActivityLevel, McAnalytics, McBrainFile, McInboxDetail, McInboxItem, McInboxKind,
    McScheduleItem, McScheduleKind, McToolStat, TimeWindow,
};
