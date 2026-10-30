//! The `/usage` header must cover the ledger it claims to summarise (#2249).
//!
//! Two defects, each pinned here:
//!
//! 1. Calls the ledger recorded without a model name were filtered out of the
//!    By Model query (`WHERE model != ''`), and the header is the sum of those
//!    rows, so 56 million tokens on this machine were invisible in both cards.
//! 2. The figure is recorded cost. Where a provider reported nothing the row
//!    holds `0.0`, so the header read as total spend while 31% of the token
//!    base carried no cost at all. The bar now names that share.

use crate::db::Database;
use crate::db::repository::usage_ledger::UsageLedgerRepository;
use crate::usage::cards::render_summary;
use crate::usage::data::{DashboardData, ModelEntry, Period, SummaryStats, UNATTRIBUTED_MODEL};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

// ── Harness ──────────────────────────────────────────────────────────────────

/// Render the real summary bar into a fresh buffer.
fn render_summary_buf(data: &DashboardData) -> Buffer {
    render_summary_buf_at(data, 120)
}

/// Render the summary bar at a chosen width.
fn render_summary_buf_at(data: &DashboardData, w: u16) -> Buffer {
    let h = 3u16;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    let area = Rect::new(0, 0, w, h);
    terminal
        .draw(|f| {
            let view =
                crate::ui_core::usage::view_model::UsageViewModel::build(data, Period::AllTime);
            render_summary(f, &view, area)
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Extract the text of a single buffer row.
fn row_text(buf: &Buffer, row: u16, width: u16) -> String {
    (0..width)
        .map(|col| {
            buf.cell((col, row))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<String>()
}

/// Whole buffer flattened to one string for `.contains`.
fn buf_text(buf: &Buffer) -> String {
    let area = buf.area;
    (0..area.height)
        .map(|row| row_text(buf, row, area.width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dashboard data with only the summary bar populated.
fn data_with(summary: SummaryStats, models: Vec<ModelEntry>) -> DashboardData {
    DashboardData {
        summary,
        models,
        ..Default::default()
    }
}

// ── Defect 1: calls with no model name ───────────────────────────────────────

#[tokio::test]
async fn calls_recorded_without_a_model_reach_the_header_and_the_model_list() {
    let home = tempfile::tempdir().expect("tempdir");
    crate::config::profile::with_home_override_async(home.path().to_path_buf(), async {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let repo = UsageLedgerRepository::new(db.pool().clone());

        // A normal call, and one the ledger holds with no model name at all.
        repo.record("s1", "zai", "glm-5.3-flash", 10_000_000, 0.30, None, None)
            .await
            .unwrap();
        repo.record("s1", "zai", "", 5_000_000, 0.25, None, None)
            .await
            .unwrap();

        let d = DashboardData::fetch(db.pool(), Period::AllTime)
            .await
            .expect("dashboard fetch");

        let bucket = d
            .models
            .iter()
            .find(|m| m.model == UNATTRIBUTED_MODEL)
            .expect("calls with no model name must still appear in By Model");
        assert_eq!(bucket.tokens, 5_000_000);
        assert_eq!(bucket.calls, 1);
        assert!(
            (bucket.cost - 0.25).abs() < 1e-9,
            "bucket keeps the recorded cost, got {}",
            bucket.cost
        );

        let model_sum: f64 = d.models.iter().map(|m| m.cost).sum();
        assert!(
            (model_sum - 0.55).abs() < 1e-9,
            "model rows must sum the whole ledger, got {model_sum}"
        );
        assert!(
            (d.summary.total_cost - 0.55).abs() < 1e-9,
            "header must equal the model rows including the bucket, got {}",
            d.summary.total_cost
        );
        assert_eq!(
            d.summary.total_tokens, 15_000_000,
            "header tokens come from the ledger, so they always covered these calls"
        );
    })
    .await;
}

// ── Defect 2: recorded cost, not spend ───────────────────────────────────────

#[tokio::test]
async fn unpriced_tokens_count_only_the_calls_that_recorded_no_cost() {
    let home = tempfile::tempdir().expect("tempdir");
    crate::config::profile::with_home_override_async(home.path().to_path_buf(), async {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let repo = UsageLedgerRepository::new(db.pool().clone());

        // Same model, so the group's ledger cost stays above zero and no table
        // estimate can stand in: the zero row is exactly what the header misses.
        repo.record("s1", "zai", "glm-5.3-flash", 2_000_000, 0.50, None, None)
            .await
            .unwrap();
        repo.record("s1", "zai", "glm-5.3-flash", 1_000_000, 0.0, None, None)
            .await
            .unwrap();

        let d = DashboardData::fetch(db.pool(), Period::AllTime)
            .await
            .expect("dashboard fetch");

        assert_eq!(
            d.summary.unpriced_tokens, 1_000_000,
            "only the zero-cost call's tokens are unpriced"
        );
        assert_eq!(d.summary.total_tokens, 3_000_000);
        assert!(
            (d.summary.total_cost - 0.50).abs() < 1e-9,
            "the header reports what was recorded, got {}",
            d.summary.total_cost
        );
    })
    .await;
}

#[test]
fn summary_bar_names_the_share_of_tokens_that_carry_no_recorded_cost() {
    let summary = SummaryStats {
        total_tokens: 1_000_000_000,
        total_cost: 21_301.0,
        session_count: 12,
        call_count: 23_348,
        unpriced_tokens: 310_000_000,
    };
    let text = buf_text(&render_summary_buf(&data_with(summary, Vec::new())));

    assert!(
        text.contains("31% unpriced"),
        "the bar must say how much of the token base carries no cost, got:\n{text}"
    );
    assert!(
        text.contains("$21301.00"),
        "the recorded figure still renders, got:\n{text}"
    );
}

#[test]
fn summary_bar_marks_a_total_that_includes_an_estimate() {
    let summary = SummaryStats {
        total_tokens: 1_000,
        total_cost: 5.0,
        session_count: 1,
        call_count: 1,
        unpriced_tokens: 0,
    };
    let models = vec![ModelEntry {
        model: "estimate-model".to_string(),
        tokens: 1_000,
        cost: 5.0,
        calls: 1,
        estimated: true,
        cache_read_tokens: None,
        cache_split_tokens: 0,
        variants: Vec::new(),
    }];
    let text = buf_text(&render_summary_buf(&data_with(summary, models)));

    assert!(
        text.contains("~$5.00"),
        "an estimated total carries the same marker as the model card, got:\n{text}"
    );
    assert!(
        !text.contains("unpriced"),
        "a ledger with nothing unpriced shows no share marker, got:\n{text}"
    );
}
#[test]
fn summary_bar_drops_the_share_rather_than_clipping_a_narrow_terminal() {
    let summary = SummaryStats {
        total_tokens: 1_000_000_000,
        total_cost: 21_301.0,
        session_count: 12,
        call_count: 23_348,
        unpriced_tokens: 310_000_000,
    };
    // The dashboard renders down to 40 columns (layout::MIN_WIDTH). The bar is
    // centred, so anything that overflows eats the version and the period off
    // both ends; the share is the part that goes instead.
    let text = buf_text(&render_summary_buf_at(&data_with(summary, Vec::new()), 40));

    assert!(
        !text.contains("unpriced"),
        "a bar too narrow for the share must drop it, not clip, got:\n{text}"
    );
}
