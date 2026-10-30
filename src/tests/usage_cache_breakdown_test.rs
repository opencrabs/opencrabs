//! Cache-breakdown money math for the /usage Cache Efficiency card (#2027).
//!
//! The card knows the cached-token volume per model but not what it cost, so
//! the Enter-to-expand view needs three numbers per model: what the cache reads
//! cost, what the cache writes cost, and what those reads would have cost at the
//! plain input rate (`saved` is the difference). [`ModelRates`] carries the
//! resolved rates; [`CacheCost`] turns token counts into those dollars.
//!
//! The rates are read from the SHIPPED table (`usage_pricing.toml.example`),
//! not a hand-built fixture, so a table edit that drops a cache rate fails here
//! instead of silently inventing one at runtime.

use crate::ui_core::usage::view_model::UsageViewModel;
use crate::usage::cards;
use crate::usage::data::{CacheStats, ModelCacheStat};
use crate::usage::pricing::{
    CacheCost, CostOrigin, DEFAULT_UNKNOWN_INPUT_PER_M, DEFAULT_UNKNOWN_OUTPUT_PER_M, ModelRates,
    PricingConfig,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

/// The shipped pricing table, parsed the same way `PricingConfig::load` parses
/// the user file. Binding the tests to the real table is the point.
fn shipped_table() -> PricingConfig {
    toml::from_str(include_str!("../../usage_pricing.toml.example"))
        .expect("shipped usage_pricing.toml.example must parse as PricingConfig")
}

const EPS: f64 = 1e-9;

fn approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < EPS,
        "expected {expected}, got {actual}"
    );
}

// ── rates_for: exact table entry ─────────────────────────────────────────────

#[test]
fn rates_for_reads_exact_table_entry() {
    let rates = shipped_table().rates_for("glm-5.3-flash");
    assert_eq!(rates.origin, CostOrigin::Priced);
    approx(rates.input_per_m, 0.15);
    approx(rates.output_per_m, 0.50);
    // The table declares cache_read_per_m = 0.03 and no cache_write_per_m, so
    // the write side must be filled with the documented 1.25x input default.
    approx(rates.cache_read_per_m, 0.03);
    approx(rates.cache_write_per_m, 0.1875);
}

// ── rates_for: both cache defaults applied ───────────────────────────────────

#[test]
fn rates_for_applies_cache_defaults_when_absent() {
    // glm-5.1 declares neither cache rate, so read = 0.1x input and
    // write = 1.25x input must both be derived.
    let rates = shipped_table().rates_for("glm-5.1");
    assert_eq!(rates.origin, CostOrigin::Priced);
    approx(rates.input_per_m, 0.95);
    approx(rates.cache_read_per_m, 0.095);
    approx(rates.cache_write_per_m, 1.1875);
}

// ── rates_for: unknown family falls back and says so ─────────────────────────

#[test]
fn rates_for_unknown_model_is_estimated() {
    let rates = shipped_table().rates_for("zzz-unlisted-model-9z");
    assert_eq!(rates.origin, CostOrigin::Estimated);
    approx(rates.input_per_m, DEFAULT_UNKNOWN_INPUT_PER_M);
    approx(rates.output_per_m, DEFAULT_UNKNOWN_OUTPUT_PER_M);
    approx(rates.cache_read_per_m, DEFAULT_UNKNOWN_INPUT_PER_M * 0.1);
    approx(rates.cache_write_per_m, DEFAULT_UNKNOWN_INPUT_PER_M * 1.25);
}

// ── CacheCost: the issue's own evidence arithmetic ───────────────────────────

/// 14,103,232 cached tokens on glm-5.3-flash. Read at $0.03/M they cost
/// $0.4231; sent as ordinary input at $0.15/M they would have cost $2.1155, so
/// the cache saved $1.6924. These are the numbers the expanded card shows.
#[test]
fn cache_cost_matches_issue_evidence() {
    const CACHED: i64 = 14_103_232;
    let rates = shipped_table().rates_for("glm-5.3-flash");
    let cost = CacheCost::compute(CACHED, 0, &rates);

    approx(cost.read_cost, 0.42309696);
    approx(cost.full_rate_cost, 2.1154848);
    approx(cost.saved, 1.69238784);
    // No cache writes reported for this run, so the write side stays $0.
    approx(cost.write_cost, 0.0);
}

// ── CacheCost: the write side is billed separately ───────────────────────────

#[test]
fn cache_cost_bills_writes_at_the_write_rate() {
    let rates = shipped_table().rates_for("glm-5.3-flash");
    let cost = CacheCost::compute(0, 2_000_000, &rates);
    // 2M write tokens at the 1.25x write rate (0.1875/M).
    approx(cost.write_cost, 0.375);
    // Nothing was read at cache rate, so nothing was "saved" on the read side.
    approx(cost.read_cost, 0.0);
    approx(cost.saved, 0.0);
}

// ── CacheCost: identity that must hold for any rates ─────────────────────────

#[test]
fn saved_is_full_rate_minus_read_cost() {
    let rates = ModelRates {
        input_per_m: 3.0,
        output_per_m: 15.0,
        cache_write_per_m: 3.75,
        cache_read_per_m: 0.3,
        origin: CostOrigin::Priced,
    };
    let cost = CacheCost::compute(1_000_000, 1_000_000, &rates);
    approx(cost.read_cost, 0.3);
    approx(cost.full_rate_cost, 3.0);
    approx(cost.saved, cost.full_rate_cost - cost.read_cost);
    approx(cost.saved, 2.7);
}

// ── The breakdown never contradicts the existing totals ──────────────────────

/// `rate_for` must agree with the billing path the ledger already uses: the
/// cache-read dollars it reports are the same cache-read dollars
/// `calculate_cost_with_cache` charges. If these two ever diverge, the expanded
/// card and the cost column tell different stories about the same tokens.
#[test]
fn cache_read_rate_agrees_with_billing_path() {
    let table = shipped_table();
    let rates = table.rates_for("glm-5.3-flash");
    let cached: u32 = 14_103_232;
    let full = table.calculate_cost_with_cache("glm-5.3-flash", 0, 0, 0, cached);
    approx(CacheCost::compute(cached as i64, 0, &rates).read_cost, full);
}

// ── The per-model SQL must carry the cache-write count (#2027) ───────────────

/// Runs the REAL per-model cache SQL (shared consts, so this cannot drift from
/// the card's own query) over a cache-heavy row set, then prices the returned
/// counts through the same `rates_for` + `CacheCost::compute` path
/// `fetch_cache_stats` uses. A model with explicit cache rates and one without
/// must both come out right, and a provider that reports no cache columns must
/// stay excluded.
#[test]
fn per_model_sql_counts_and_money_match_hand_arithmetic() {
    use crate::usage::data::{
        CACHE_CAPABLE_WHERE, CACHE_COST_SELECT_COLS, CACHE_MODEL_EXPR, CACHE_STATS_SELECT_COLS,
        CacheStats, ModelCacheStat,
    };
    use crate::usage::pricing::CostOrigin;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT, model TEXT);
         CREATE TABLE messages (
            session_id TEXT, input_tokens INTEGER,
            cache_creation_tokens INTEGER, cache_read_tokens INTEGER
         );
         INSERT INTO sessions VALUES ('s1','glm-5.3-flash');
         INSERT INTO sessions VALUES ('s2','glm-5.1');
         INSERT INTO sessions VALUES ('s3','local-gguf');
         -- glm-5.3-flash: 1M read, 100K written, 500K plain input.
         INSERT INTO messages VALUES ('s1', 500000, 100000, 1000000);
         -- glm-5.1: 2M read, nothing written, the table declares no cache rates.
         INSERT INTO messages VALUES ('s2', 0, 0, 2000000);
         -- non-caching provider: both cache columns NULL, must be excluded.
         INSERT INTO messages VALUES ('s3', 100, NULL, NULL);",
    )
    .unwrap();

    let sql = format!(
        "SELECT {CACHE_MODEL_EXPR}, {CACHE_STATS_SELECT_COLS}, {CACHE_COST_SELECT_COLS} \
         FROM messages m JOIN sessions s ON m.session_id = s.id \
         WHERE {CACHE_CAPABLE_WHERE} GROUP BY {CACHE_MODEL_EXPR}"
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let raw: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    let table = shipped_table();
    let mut per_model: Vec<ModelCacheStat> = raw
        .into_iter()
        .map(|(model, cached, total, cache_creation)| {
            let rates = table.rates_for(&model);
            ModelCacheStat {
                model,
                cache_hit_pct: cached as f64 / total as f64 * 100.0,
                cached_tokens: cached,
                cache_creation_tokens: cache_creation,
                cost: CacheCost::compute(cached, cache_creation, &rates),
                input_per_m: rates.input_per_m,
                cache_read_per_m: rates.cache_read_per_m,
                estimated: rates.origin == CostOrigin::Estimated,
            }
        })
        .collect();
    per_model.sort_by(|a, b| a.model.cmp(&b.model));

    assert_eq!(
        per_model.len(),
        2,
        "the non-caching model must be excluded: {per_model:?}"
    );

    // glm-5.1 has no cache_read_per_m in the table, so the 0.1x default applies.
    let low = &per_model[0];
    assert_eq!(low.model, "glm-5.1");
    assert_eq!(low.cached_tokens, 2_000_000);
    assert_eq!(low.cache_creation_tokens, 0);
    assert!(!low.estimated);
    approx(low.cost.read_cost, 0.19);
    approx(low.cost.write_cost, 0.0);
    approx(low.cost.full_rate_cost, 1.90);
    approx(low.cost.saved, 1.71);

    // glm-5.3-flash writes 100K at the 0.1875 write rate.
    let flash = &per_model[1];
    assert_eq!(flash.model, "glm-5.3-flash");
    assert_eq!(flash.cached_tokens, 1_000_000);
    assert_eq!(flash.cache_creation_tokens, 100_000);
    assert!((flash.cache_hit_pct - 62.5).abs() < 1e-9);
    approx(flash.cost.read_cost, 0.03);
    approx(flash.cost.write_cost, 0.01875);
    approx(flash.cost.full_rate_cost, 0.15);
    approx(flash.cost.saved, 0.12);

    // The TOTAL line adds the rows, which only holds because every row prices
    // the same quantity.
    let totals = CacheStats {
        per_model,
        ..CacheStats::default()
    }
    .cost_totals();
    approx(totals.read_cost, 0.22);
    approx(totals.write_cost, 0.01875);
    approx(totals.full_rate_cost, 2.05);
    approx(totals.saved, 1.83);
}

// ── The Enter overlay renders what the card could not show (#2027) ───────────

/// The card the user pressed Enter on: 14.1M cached reads on `glm-5.3-flash`,
/// priced from the shipped table (the issue's own evidence).
fn breakdown_cache() -> CacheStats {
    let rates = shipped_table().rates_for("glm-5.3-flash");
    let cached = 14_103_232;
    CacheStats {
        cache_hit_pct: 97.0,
        cached_tokens: cached,
        total_input_tokens: 14_508_042,
        per_model: vec![ModelCacheStat {
            model: "glm-5.3-flash".to_string(),
            cache_hit_pct: 97.0,
            cached_tokens: cached,
            cache_creation_tokens: 0,
            cost: CacheCost::compute(cached, 0, &rates),
            input_per_m: rates.input_per_m,
            cache_read_per_m: rates.cache_read_per_m,
            estimated: rates.origin == CostOrigin::Estimated,
        }],
    }
}

/// Render `draw` into a TestBackend and return the buffer as one string.
fn render_text(width: u16, height: u16, draw: impl FnOnce(&mut ratatui::Frame, Rect)) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            draw(f, area);
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

/// Wrap raw cache data in a `DashboardData` and build the shared view
/// model, the way `dashboard::render` does before drawing.
fn view_with_cache(cache: Option<CacheStats>) -> crate::ui_core::usage::view_model::UsageViewModel {
    UsageViewModel::build(
        &crate::usage::data::DashboardData {
            cache,
            ..Default::default()
        },
        crate::usage::data::Period::AllTime,
    )
}

/// The overlay shows the cached token volume and the money the cache saved,
/// per model, under a TOTAL row. The money columns come from
/// [`CacheCost::compute`] (pinned by the tests above): read $0.423, full-rate
/// $2.12, saved $1.69.
#[test]
fn breakdown_overlay_shows_cached_tokens_and_saved_money() {
    let view = view_with_cache(Some(breakdown_cache()));
    let text = render_text(100, 30, |f, area| {
        cards::render_cache_breakdown(f, view.cache.as_ref(), area)
    });

    assert!(text.contains("Cache Efficiency: prices"), "title:\n{text}");
    assert!(text.contains("glm-5.3-flash"), "model name:\n{text}");
    assert!(text.contains("14.1M"), "cached token count:\n{text}");
    assert!(text.contains("saved"), "saved column:\n{text}");
    assert!(text.contains("$1.69"), "saved money:\n{text}");
    assert!(text.contains("TOTAL"), "total row:\n{text}");
    assert!(
        text.contains("97% of input tokens were cache reads"),
        "cache-read share:\n{text}"
    );
}

/// Without cache data the overlay says so instead of drawing an empty table.
#[test]
fn breakdown_overlay_says_so_when_there_is_no_cache_data() {
    let view = view_with_cache(Some(CacheStats::default()));
    let text = render_text(100, 30, |f, area| {
        cards::render_cache_breakdown(f, view.cache.as_ref(), area)
    });
    assert!(text.contains("No cache data"), "empty overlay:\n{text}");
}

/// Enter on the Cache Efficiency card opens the prices; Esc closes them and
/// leaves the user on the dashboard.
#[test]
fn enter_opens_the_prices_and_esc_closes_them() {
    use crate::ui_core::usage::input::CACHE_CARD;
    use crate::usage::dashboard::{DashboardState, UsageKeyOutcome, decide};

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let mut state = DashboardState {
        focused_card: CACHE_CARD,
        ..DashboardState::default()
    };

    assert_eq!(
        decide(&mut state, press(KeyCode::Enter)),
        UsageKeyOutcome::Consumed
    );
    assert!(state.cache_detail_open, "Enter opens the breakdown");

    assert_eq!(
        decide(&mut state, press(KeyCode::Esc)),
        UsageKeyOutcome::Consumed
    );
    assert!(!state.cache_detail_open, "Esc closes the breakdown");
}

/// The dashboard draws the breakdown over the grid and keeps the footer on
/// screen, so the key that closes it is always visible.
#[test]
fn dashboard_draws_the_breakdown_over_the_grid_and_footer_stays() {
    let mut state = crate::usage::dashboard::DashboardState {
        focused_card: crate::ui_core::usage::input::CACHE_CARD,
        cache_detail_open: true,
        ..crate::usage::dashboard::DashboardState::default()
    };
    state.data.cache = Some(breakdown_cache());

    let text = render_text(100, 40, |f, area| {
        crate::usage::dashboard::render(f, &state, area)
    });

    assert!(
        text.contains("glm-5.3-flash"),
        "breakdown model row:\n{text}"
    );
    assert!(text.contains("$1.69"), "breakdown saved money:\n{text}");
    assert!(
        text.contains("Esc: close"),
        "the way out must stay:\n{text}"
    );
    assert!(
        !text.contains("Enter: details"),
        "Enter does nothing while the breakdown is open:\n{text}"
    );
    assert!(
        !text.contains("T/W/M/A: period"),
        "period keys are inert while the breakdown is open:\n{text}"
    );
}
