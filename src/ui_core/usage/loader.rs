//! One call from a database to a ready-to-render usage view.
//!
//! A non-terminal frontend should not have to learn about connection pools,
//! migrations or the async runtime just to draw the dashboard. These two
//! functions are that single entry point, and they run the same
//! [`DashboardData::fetch`] the TUI runs, so both frontends report the same
//! numbers for the same period.
//!
//! Nothing here touches a terminal: no ratatui types, no crossterm events. The
//! result is a [`UsageViewModel`], which is presentation data a GUI can lay out
//! with its own widgets.

use std::path::Path;

use anyhow::{Context, Result};

use crate::db::{Database, Pool};

use super::data::{DashboardData, Period};
use super::view_model::UsageViewModel;

/// Build the usage view from an already-open pool.
///
/// A frontend that keeps a pool alive for its other screens uses this, so the
/// dashboard costs one query round instead of a fresh connection.
pub async fn usage_view_from_pool(pool: &Pool, period: Period) -> Result<UsageViewModel> {
    let data = DashboardData::fetch(pool, period)
        .await
        .with_context(|| format!("failed to read usage data for {}", period.label()))?;
    Ok(UsageViewModel::build(&data, period))
}

/// Open the database at `db_path` and build the usage view for `period`.
///
/// Deliberately does NOT run migrations. The dashboard reads a database that
/// another process (the daemon, the TUI) owns and keeps current; a reader that
/// quietly rewrote the schema would race that owner. A database whose schema is
/// too old surfaces as an error here instead.
pub async fn usage_view(db_path: &Path, period: Period) -> Result<UsageViewModel> {
    let db = Database::connect(db_path)
        .await
        .with_context(|| format!("failed to open the usage database at {}", db_path.display()))?;
    usage_view_from_pool(db.pool(), period).await
}

#[cfg(test)]
mod tests {
    //! The loader is the GUI's only door into usage data, so these pin the two
    //! promises it makes: identical numbers to the TUI's own fetch-and-build
    //! sequence, and a view that renders (never panics) on a database with no
    //! usage in it yet. Fixtures are synthetic and carry no user identifiers.

    use super::*;
    use crate::services::{ServiceContext, SessionService};

    /// Tokens and cost recorded by every fixture below.
    const TOKENS: i64 = 1_234;
    const COST: f64 = 0.5678;
    const PROVIDER: &str = "zai";
    const MODEL: &str = "glm-5.3-flash";

    /// The TUI's own sequence, spelled out: fetch the period, build the view.
    /// `src/usage/dashboard.rs::render` does exactly this per draw.
    async fn tui_path(pool: &Pool, period: Period) -> UsageViewModel {
        let data = DashboardData::fetch(pool, period)
            .await
            .expect("dashboard fetch");
        UsageViewModel::build(&data, period)
    }

    #[tokio::test]
    async fn loader_reports_the_same_numbers_the_tui_renders() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
            let db = Database::connect_in_memory().await.expect("in-memory DB");
            db.run_migrations().await.expect("migrations");
            let service = SessionService::new(ServiceContext::new(db.pool().clone()));
            let session = service
                .create_session(Some("loader fixture".to_string()))
                .await
                .expect("session");
            service
                .update_session_usage(session.id, TOKENS, COST, PROVIDER, MODEL, None, None)
                .await
                .expect("record usage");

            let via_loader = usage_view_from_pool(db.pool(), Period::AllTime)
                .await
                .expect("loader");
            let via_tui = tui_path(db.pool(), Period::AllTime).await;

            // Every pinned summary cell must match the TUI's own build.
            assert_eq!(via_loader.summary.tokens_str, via_tui.summary.tokens_str);
            assert_eq!(via_loader.summary.cost_str, via_tui.summary.cost_str);
            assert_eq!(
                via_loader.summary.sessions_str,
                via_tui.summary.sessions_str
            );
            assert_eq!(via_loader.summary.calls_str, via_tui.summary.calls_str);
            assert_eq!(
                via_loader.summary.unpriced_marker,
                via_tui.summary.unpriced_marker
            );
            assert_eq!(via_loader.period_label, via_tui.period_label);

            // And the content is the recorded row, not an empty shell.
            assert_eq!(
                via_loader.models.len(),
                1,
                "one model row: {:#?}",
                via_loader.models
            );
            assert!(
                via_loader.summary.cost_str.contains('$'),
                "cost cell must carry a currency marker, got {:?}",
                via_loader.summary.cost_str
            );
            assert!(
                !via_loader.summary.tokens_str.is_empty(),
                "tokens cell must be filled"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn loader_opens_a_database_by_path() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
            let db_path = home.path().join("usage.db");

            // Seed through one connection, then read through the loader's own.
            {
                let db = Database::connect(&db_path).await.expect("connect");
                db.run_migrations().await.expect("migrations");
                let service = SessionService::new(ServiceContext::new(db.pool().clone()));
                let session = service
                    .create_session(Some("path fixture".to_string()))
                    .await
                    .expect("session");
                service
                    .update_session_usage(session.id, TOKENS, COST, PROVIDER, MODEL, None, None)
                    .await
                    .expect("record usage");
            }

            let view = usage_view(&db_path, Period::AllTime)
                .await
                .expect("loader by path");
            assert_eq!(view.models.len(), 1, "the file database must be read back");
            assert_eq!(view.period_label, Period::AllTime.label());
        })
        .await;
    }

    #[tokio::test]
    async fn a_database_with_no_usage_still_yields_a_renderable_view() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
            let db = Database::connect_in_memory().await.expect("in-memory DB");
            db.run_migrations().await.expect("migrations");

            // First run of a fresh install: nothing recorded yet. The GUI must
            // be able to draw this instead of crashing on an empty ledger.
            let view = usage_view_from_pool(db.pool(), Period::AllTime)
                .await
                .expect("loader on an empty database");

            assert!(
                view.models.is_empty(),
                "no models recorded: {:#?}",
                view.models
            );
            assert!(view.daily.is_empty(), "no days recorded");
            assert!(
                !view.summary.tokens_str.is_empty(),
                "the cell is filled, not absent"
            );
            assert!(view.cache.is_none(), "no cache rows means no cache card");
        })
        .await;
    }

    #[tokio::test]
    async fn all_four_periods_fetch_clean_on_a_seeded_ledger() {
        // The first real window died with SIGILL on the period keys (w/m/a).
        // This runs every period's query headlessly, so a panic anywhere in
        // that path is a failing test instead of a dead window.
        let home = tempfile::tempdir().expect("tempdir");
        crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
            let db = Database::connect_in_memory().await.expect("in-memory DB");
            db.run_migrations().await.expect("migrations");
            let service = SessionService::new(ServiceContext::new(db.pool().clone()));
            let session = service
                .create_session(Some("period fixture".to_string()))
                .await
                .expect("session");
            service
                .update_session_usage(session.id, TOKENS, COST, PROVIDER, MODEL, None, None)
                .await
                .expect("record usage");

            for period in [Period::Today, Period::Week, Period::Month, Period::AllTime] {
                let view = usage_view_from_pool(db.pool(), period)
                    .await
                    .unwrap_or_else(|e| panic!("{period:?} fetch failed: {e:#}"));
                assert_eq!(view.period_label, period.label());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn the_real_ledger_fetches_for_every_period_when_present() {
        // Reads this machine's actual ledger read-only when it exists, so the
        // real data shapes (many days, many models) cross every period query.
        // Skips silently where there is no ledger, e.g. on CI.
        let db_path = crate::config::opencrabs_home().join("opencrabs.db");
        if !db_path.exists() {
            return;
        }
        for period in [Period::Today, Period::Week, Period::Month, Period::AllTime] {
            let view = usage_view(&db_path, period)
                .await
                .unwrap_or_else(|e| panic!("{period:?} fetch on the real ledger failed: {e:#}"));
            assert_eq!(view.period_label, period.label());
        }
    }
}
