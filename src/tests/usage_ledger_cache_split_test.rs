//! The ledger records the cache-read split per row (#2027).
//!
//! `usage_ledger` kept only a token total and a cost, so the By Model card
//! could not say how much of a model's tokens were served from cache. These
//! tests pin the write path (service into ledger), the read path (ledger into
//! the model rows) and the rendered card.
//!
//! The distinction that matters throughout: a row that recorded no split at
//! all is not the same fact as a row that recorded zero cache reads. The
//! first renders blank, the second renders 0%.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use crate::usage::cards;
use crate::usage::data::{DashboardData, ModelEntry, Period};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// The run from the issue: 14,508,042 tokens, 14,103,232 of them cache reads,
/// billed $0.5246 by the provider. The share of tokens is 97.2%.
const HEAVY_TOKENS: i64 = 14_508_042;
const HEAVY_READS: i64 = 14_103_232;
const HEAVY_COST: f64 = 0.5246;

/// Run `usage` through the real service into a real ledger under a temp home,
/// then read the By Model rows back the way the dashboard does.
async fn model_rows(usage: Vec<(i64, f64, Option<i64>)>) -> Vec<ModelEntry> {
    let home = tempfile::tempdir().expect("tempdir");
    crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let service = SessionService::new(ServiceContext::new(db.pool().clone()));
        let session = service
            .create_session(Some("cache split".to_string()))
            .await
            .expect("session");
        for (tokens, cost, cache_read) in usage {
            service
                .update_session_usage(
                    session.id,
                    tokens,
                    cost,
                    "zai",
                    "glm-5.3-flash",
                    cache_read,
                    cache_read.map(|_| tokens),
                )
                .await
                .expect("record usage");
        }
        DashboardData::fetch(db.pool(), Period::AllTime)
            .await
            .expect("dashboard fetch")
            .models
    })
    .await
}

#[tokio::test]
async fn a_cache_heavy_call_carries_its_read_count_and_share() {
    let models = model_rows(vec![(HEAVY_TOKENS, HEAVY_COST, Some(HEAVY_READS))]).await;
    assert_eq!(models.len(), 1, "one model row expected: {models:?}");
    let m = &models[0];
    assert_eq!(
        m.cache_read_tokens,
        Some(HEAVY_READS),
        "the ledger must store the cache-read count the provider reported"
    );
    let pct = m
        .cache_read_pct()
        .expect("a recorded split must yield a share");
    assert!(
        (pct - 97.2).abs() < 0.05,
        "14,103,232 of 14,508,042 tokens is 97.2%, got {pct}"
    );
}

#[tokio::test]
async fn a_row_with_no_recorded_split_reports_none_not_zero() {
    // This is the shape every row written before the migration has.
    let models = model_rows(vec![(1_000, 0.01, None)]).await;
    let m = &models[0];
    assert_eq!(
        m.cache_read_tokens, None,
        "a turn that recorded no cache usage must not be stored as a zero"
    );
    assert_eq!(
        m.cache_read_pct(),
        None,
        "no split means no percentage, never a made-up 0%"
    );
}

#[tokio::test]
async fn a_recorded_zero_is_zero_percent_not_a_missing_split() {
    let models = model_rows(vec![(1_000, 0.01, Some(0))]).await;
    let m = &models[0];
    assert_eq!(m.cache_read_tokens, Some(0));
    assert_eq!(
        m.cache_read_pct(),
        Some(0.0),
        "a provider that reported no cache reads measured 0%, and that is data"
    );
}

#[tokio::test]
async fn the_share_divides_by_the_rows_that_recorded_a_split() {
    // One call with a split, one legacy row without one. Dividing by the
    // model's whole token total would report 5% instead of the 50% those
    // measured tokens actually saw.
    let models = model_rows(vec![
        (1_000_000, 0.20, Some(500_000)),
        (9_000_000, 0.30, None),
    ])
    .await;
    assert_eq!(models.len(), 1, "both calls are the same model: {models:?}");
    let m = &models[0];
    assert_eq!(m.tokens, 10_000_000);
    assert_eq!(m.cache_split_tokens, 1_000_000);
    let pct = m.cache_read_pct().expect("share");
    assert!(
        (pct - 50.0).abs() < 0.05,
        "500,000 of the 1,000,000 recorded tokens were cache reads, got {pct}"
    );
}

/// Render `render_models` into a TestBackend and return the buffer as text.
fn model_card_text(models: &[ModelEntry]) -> String {
    let backend = TestBackend::new(90, 6);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            cards::render_models(f, models, area, false);
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

fn entry(model: &str, cached: Option<i64>, split_tokens: i64) -> ModelEntry {
    ModelEntry {
        model: model.to_string(),
        tokens: split_tokens.max(1),
        cost: HEAVY_COST,
        calls: 1,
        estimated: false,
        cache_read_tokens: cached,
        cache_split_tokens: split_tokens,
        variants: Vec::new(),
    }
}

#[test]
fn by_model_card_renders_the_cache_share() {
    let models = vec![entry("glm-5.3-flash", Some(HEAVY_READS), HEAVY_TOKENS)];
    let text = model_card_text(&models);
    assert!(
        text.contains("97% cache"),
        "the card must show the cache share, buffer was:\n{text}"
    );
}

#[test]
fn by_model_card_omits_the_cache_column_without_a_split() {
    // Nothing in the ledger recorded a split, so there is no column to render
    // and no zero to imply.
    let models = vec![entry("glm-5.3-flash", None, 0)];
    let text = model_card_text(&models);
    assert!(
        !text.contains("% cache"),
        "no split means no cache column, buffer was:\n{text}"
    );
}

#[test]
fn by_model_card_leaves_the_cell_blank_for_a_model_without_a_split() {
    // One model has a split, the other does not. The column is present, and
    // only the measured model fills it.
    let models = vec![
        entry("glm-5.3-flash", Some(HEAVY_READS), HEAVY_TOKENS),
        entry("kimi-k3", None, 0),
    ];
    let text = model_card_text(&models);
    assert_eq!(
        text.matches("% cache").count(),
        1,
        "exactly the measured model may show a share, buffer was:\n{text}"
    );
}
