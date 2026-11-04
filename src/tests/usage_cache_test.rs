//! Cache Efficiency Card Tests
//!
//! Tests for CacheStats struct, percentage calculation, and edge cases.

use crate::usage::data::CacheStats;

// ─────────────────────────────────────────────────────────────────────────────
// CacheStats struct tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cache_stats_default_is_zero() {
    let stats = CacheStats::default();
    assert_eq!(stats.cache_hit_pct, 0.0);
    assert_eq!(stats.cached_tokens, 0);
    assert_eq!(stats.total_input_tokens, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache hit percentage calculation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cache_hit_pct_simple_case() {
    // 800 cached out of 1000 total = 80%
    let cached = 800i64;
    let total = 1000i64;
    let pct = (cached as f64 / total as f64) * 100.0;
    assert!((pct - 80.0).abs() < 0.01);
}

#[test]
fn cache_hit_pct_zero_cached() {
    let cached = 0i64;
    let total = 1000i64;
    let pct = (cached as f64 / total as f64) * 100.0;
    assert!((pct - 0.0).abs() < 0.01);
}

#[test]
fn cache_hit_pct_all_cached() {
    let cached = 1000i64;
    let total = 1000i64;
    let pct = (cached as f64 / total as f64) * 100.0;
    assert!((pct - 100.0).abs() < 0.01);
}

#[test]
fn cache_hit_pct_partial_cache() {
    // Simulating real Anthropic numbers:
    // input_tokens=1000, cache_creation=80000, cache_read=15000
    // total_input = 1000 + 80000 + 15000 = 96000
    // cached = 15000
    // pct = 15000/96000 = 15.625%
    let cached = 15000i64;
    let total = 96000i64;
    let pct = (cached as f64 / total as f64) * 100.0;
    assert!((pct - 15.625).abs() < 0.01);
}

// ─────────────────────────────────────────────────────────────────────────────
// CacheStats construction with realistic data
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cache_stats_construction() {
    let stats = CacheStats {
        cache_hit_pct: 67.5,
        cached_tokens: 1_200_000,
        total_input_tokens: 1_800_000,
        per_model: Vec::new(),
    };
    assert!((stats.cache_hit_pct - 67.5).abs() < 0.01);
    assert_eq!(stats.cached_tokens, 1_200_000);
    assert_eq!(stats.total_input_tokens, 1_800_000);
}

#[test]
fn cache_stats_no_cache_data() {
    // When there's no cache data, DashboardData.cache should be None
    // This simulates a fresh install with no messages yet
    let stats: Option<CacheStats> = None;
    assert!(stats.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// Color thresholds (matching render_cache_efficiency logic)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cache_pct_green_threshold() {
    // >= 60% should be green
    let pct = 67.0;
    assert!(pct >= 60.0);
}

#[test]
fn cache_pct_yellow_threshold() {
    // 30-60% should be yellow
    let pct = 45.0;
    assert!((30.0..60.0).contains(&pct));
}

#[test]
fn cache_pct_red_threshold() {
    // < 30% should be red
    let pct = 15.0;
    assert!(pct < 30.0);
}

#[test]
fn cache_pct_boundary_60() {
    // Exactly 60% should be green (>= 60)
    let pct = 60.0;
    assert!(pct >= 60.0);
}

#[test]
fn cache_pct_boundary_30() {
    // Exactly 30% should be yellow (>= 30 and < 60)
    let pct = 30.0;
    assert!((30.0..60.0).contains(&pct));
}

// ─────────────────────────────────────────────────────────────────────────────
// DashboardData cache field
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn dashboard_data_cache_field_defaults_to_none() {
    use crate::usage::data::DashboardData;
    let d = DashboardData::default();
    assert!(d.cache.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// SQL aggregation — the card measures caching-CAPABLE efficiency: of the input
// on providers that actually do caching, how much hit cache. Requests on models
// with no caching at all (NULL cache columns) are excluded so they don't dilute
// the number; cache MISSES (value 0) stay in the denominator. Runs the ACTUAL
// production SQL (shared consts) so the test can't drift from the card.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn aggregation_is_caching_capable_only_and_coalesces_nulls() {
    use crate::usage::data::{CACHE_CAPABLE_WHERE, CACHE_STATS_SELECT_COLS};

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (
            input_tokens INTEGER,
            cache_creation_tokens INTEGER,
            cache_read_tokens INTEGER
         );
         -- caching HIT: 90 of its input came from cache. included.
         INSERT INTO messages VALUES (10, 0, 90);
         -- caching MISS: provider reported 0 cached (not NULL). included, and it
         -- correctly drags efficiency DOWN by adding 50 to the denominator only.
         INSERT INTO messages VALUES (50, 0, 0);
         -- caching row with a partial NULL (provider omitted cache_creation):
         -- still included; COALESCE treats the NULL as 0.
         INSERT INTO messages VALUES (20, NULL, 80);
         -- NON-caching model (local llama.cpp etc.): both NULL. EXCLUDED so it
         -- doesn't dilute the caching-efficiency number.
         INSERT INTO messages VALUES (100, NULL, NULL);",
    )
    .unwrap();

    let sql = format!("SELECT {CACHE_STATS_SELECT_COLS} FROM messages WHERE {CACHE_CAPABLE_WHERE}");
    let (cached, total): (i64, i64) = conn
        .query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();

    // cached = 90 + 0 + 80 = 170 (the excluded non-caching row adds nothing).
    assert_eq!(cached, 170);
    // total = (10+0+90) + (50+0+0) + (20+0+80) = 250.
    // The non-caching row's 100 input is NOT counted (no caching to measure);
    // the cache-miss row's 50 IS counted.
    assert_eq!(
        total, 250,
        "miss is in the denominator; the non-caching NULL row is excluded"
    );

    let pct = 100.0 * cached as f64 / total as f64;
    assert!((pct - 68.0).abs() < 0.01, "expected 68%, got {pct}");
}

#[test]
fn per_model_query_strips_namespace_and_excludes_non_caching() {
    use crate::usage::data::{
        CACHE_CAPABLE_WHERE, CACHE_COST_SELECT_COLS, CACHE_MODEL_EXPR, CACHE_STATS_SELECT_COLS,
    };

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT, model TEXT);
         CREATE TABLE messages (
            session_id TEXT, input_tokens INTEGER,
            cache_creation_tokens INTEGER, cache_read_tokens INTEGER
         );
         INSERT INTO sessions VALUES ('s1','vendor/ModelA'), ('s2','ModelB'), ('s3','local-gguf');
         INSERT INTO messages VALUES ('s1', 10,  0,    90);   -- ModelA: caching, 90%
         INSERT INTO messages VALUES ('s2', 50,  0,    50);   -- ModelB: caching, 50%
         INSERT INTO messages VALUES ('s3', 100, NULL, NULL); -- non-caching: excluded",
    )
    .unwrap();

    let sql = format!(
        "SELECT {CACHE_MODEL_EXPR}, {CACHE_STATS_SELECT_COLS}, {CACHE_COST_SELECT_COLS} \
         FROM messages m JOIN sessions s ON m.session_id = s.id \
         WHERE {CACHE_CAPABLE_WHERE} GROUP BY {CACHE_MODEL_EXPR}"
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut rows: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get(1)?,
                r.get(2)?,
                // The cache-write sum the #2027 money breakdown prices.
                r.get(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // Sort highest hit-rate first, like production.
    rows.sort_by(|a, b| {
        (b.1 as f64 / b.2 as f64)
            .partial_cmp(&(a.1 as f64 / a.2 as f64))
            .unwrap()
    });

    // The non-caching model (NULL cache columns) is excluded entirely.
    assert_eq!(
        rows.len(),
        2,
        "non-caching model must be excluded; got {rows:?}"
    );
    // `vendor/ModelA` is grouped namespace-stripped as `ModelA`, highest first.
    // Tuple is (model, cached, total_input, cache_creation).
    assert_eq!(rows[0], ("ModelA".to_string(), 90, 100, 0));
    assert_eq!(rows[1], ("ModelB".to_string(), 50, 100, 0));
}
