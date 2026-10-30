//! Per-model cost reads the usage ledger, not the cache-blind estimate.
//!
//! A cache-heavy run (14.5M tokens, almost all cache reads) recorded its real
//! cost in the ledger. The By Model card used to re-price the raw token total
//! at an 80/20 input/output split, which ignores cache, and the header followed
//! that figure. These tests pin the ledger sum on both the model rows and the
//! header.

use crate::db::Database;
use crate::db::repository::usage_ledger::UsageLedgerRepository;
use crate::usage::data::{DashboardData, Period};

#[tokio::test]
async fn model_rows_and_header_sum_the_ledger_cost() {
    let home = tempfile::tempdir().expect("tempdir");
    crate::config::profile::with_home_override_async(home.path().to_path_buf(), async {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let repo = UsageLedgerRepository::new(db.pool().clone());

        // Two calls of one model. Their recorded costs sum to the figure the
        // provider path produced for the whole run.
        repo.record("s1", "zai", "glm-5.3-flash", 10_000_000, 0.30, None, None)
            .await
            .unwrap();
        repo.record("s1", "zai", "glm-5.3-flash", 4_540_704, 0.2246, None, None)
            .await
            .unwrap();

        let d = DashboardData::fetch(db.pool(), Period::AllTime)
            .await
            .expect("dashboard fetch");

        let model_sum: f64 = d.models.iter().map(|m| m.cost).sum();
        assert!(
            (model_sum - 0.5246).abs() < 1e-9,
            "model rows must sum the ledger, got {model_sum}"
        );
        assert!(
            (d.summary.total_cost - 0.5246).abs() < 1e-9,
            "header must match the model rows, got {}",
            d.summary.total_cost
        );
    })
    .await;
}
