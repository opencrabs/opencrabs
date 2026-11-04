//! The shipped table must price the models the ledger actually records.
//!
//! /usage fills a row from the table only when the ledger recorded no cost, so
//! a missing row is invisible spend and a stale row is a wrong total. The
//! models below were all present in a real ledger before they had a table
//! entry: mimo-v2.5-pro alone carried 4.36B unpriced tokens.
//!
//! Each assertion names the id as the ledger spells it (dot forms from the
//! provider APIs, hyphen forms from older rows) plus the list card the vendor
//! publishes. Cache rates are asserted where a vendor publishes them, because
//! the 0.1x default is wrong for most of these (DeepSeek bills reads at 2% of
//! input, Kimi at 10%, Anthropic at 2.5% on Opus 5.5).

use crate::usage::pricing::{CostOrigin, PricingConfig};

/// The shipped table, parsed exactly the way `PricingConfig::load` parses the
/// user file.
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

#[track_caller]
fn assert_rates(
    model: &str,
    input: f64,
    output: f64,
    cache_write: Option<f64>,
    cache_read: Option<f64>,
) {
    let rates = shipped_table().rates_for(model);
    assert_eq!(
        rates.origin,
        CostOrigin::Priced,
        "{model} must resolve to a table row, not the conservative default"
    );
    approx(rates.input_per_m, input);
    approx(rates.output_per_m, output);
    // Defaults: write = 1.25x input, read = 0.1x input. Assert them only when
    // the row declares its own, since derived values are covered elsewhere.
    if let Some(write) = cache_write {
        approx(rates.cache_write_per_m, write);
    }
    if let Some(read) = cache_read {
        approx(rates.cache_read_per_m, read);
    }
}

// ── Anthropic ───────────────────────────────────────────────────────────────

#[test]
fn opus_5_5_is_cheaper_than_opus_5_and_wins_the_match() {
    // $4/$20, cache write $5, cache read $0.20. Opus 5 sits right behind it at
    // $5/$25, so this also proves the more specific row wins the prefix match.
    assert_rates("claude-opus-5-5", 4.0, 20.0, Some(5.0), Some(0.20));
    assert_rates("opus-5-5", 4.0, 20.0, Some(5.0), Some(0.20));
    assert_rates("opus-5", 5.0, 25.0, Some(6.25), Some(0.50));
    assert_rates("opus-4-8", 5.0, 25.0, None, None);
}

#[test]
fn fable_5_1_keeps_its_cache_read_rate() {
    // $10/$50, 5m write $12.50, read $0.25. The pre-existing `claude-fable`
    // row carried no cache rates, so reads fell to the 0.1x default ($1.00).
    assert_rates("claude-fable-5-1", 10.0, 50.0, Some(12.5), Some(0.25));
    assert_rates("fable-5-1", 10.0, 50.0, Some(12.5), Some(0.25));
    assert_rates("fable", 10.0, 50.0, Some(12.5), Some(0.25));
}

#[test]
fn haiku_5_5_bills_the_100k_card() {
    // $0.10/$0.50 with $0.01 cache reads for prompts up to 100K. A prompt over
    // 100K bills $0.50/$2.50 and no single row can express both.
    assert_rates("claude-haiku-5-5", 0.10, 0.50, None, Some(0.01));
    assert_rates("haiku-5-5", 0.10, 0.50, None, Some(0.01));
    assert_rates("haiku-4-5", 1.0, 5.0, None, Some(0.10));
}

// ── DeepSeek ────────────────────────────────────────────────────────────────

#[test]
fn deepseek_v4_family_resolves_at_off_peak_list() {
    // V4.1 Flash $0.15/$0.60, V4 Pro $0.66/$1.98. Peak hours bill double; the
    // stored card is off-peak because it covers the rest of the week.
    assert_rates("deepseek-v4.1-flash", 0.15, 0.60, None, Some(0.003));
    assert_rates("deepseek-4.1-flash", 0.15, 0.60, None, Some(0.003));
    assert_rates("deepseek-v4-flash", 0.15, 0.60, None, Some(0.003));
    assert_rates("deepseek-v4-pro", 0.66, 1.98, None, Some(0.022));
}

// ── Moonshot ────────────────────────────────────────────────────────────────

#[test]
fn kimi_k3_1_pro_beats_the_k3_row() {
    // $2/$10 with $0.20 cache reads and a $2 5m cache write. Kimi K3 itself
    // stays at $3/$15, so the specific row has to win.
    assert_rates("kimi-k3.1-pro", 2.0, 10.0, Some(2.0), Some(0.20));
    // The bare id the ledger writes is contained in the highspeed prefix, so the
    // two rows tie on overlap and the tie breaks to the dearer card. It
    // over-states that model, which is the safe direction, and the test pins the
    // behaviour instead of pretending the table can tell the ids apart.
    assert_rates("k3.1-pro", 4.0, 20.0, Some(4.0), Some(0.20));
    assert_rates("kimi-k3", 3.0, 15.0, None, None);
}

#[test]
fn kimi_k3_1_highspeed_and_256k_keep_their_cards() {
    // Highspeed doubles input/output and cache writes. The 256K-context K3
    // variant bills the plain K3 card.
    assert_rates("kimi-k3.1-pro-highspeed", 4.0, 20.0, Some(4.0), Some(0.20));
    assert_rates("k3-256k", 3.0, 15.0, None, Some(0.30));
}

// ── Qwen ────────────────────────────────────────────────────────────────────

#[test]
fn qwen3_8_flash_and_omni_share_the_international_card() {
    // $0.15/$0.47, cache write $0.20, cache read $0.016. Both spellings are
    // live in the ledger (dot forms from the API, hyphen forms from older rows).
    for id in [
        "qwen3.8-flash",
        "qwen3.8-flash-next",
        "qwen-3.8-flash",
        "qwen3.8-omni-flash",
        "qwen-3.8-omni-flash",
    ] {
        assert_rates(id, 0.15, 0.47, Some(0.20), Some(0.016));
    }
}

#[test]
fn qwen3_7_max_and_plus_are_priced() {
    // Model Studio list, Singapore region: Max $2.50/$7.50, Plus $0.40/$1.60.
    for id in ["qwen3.7-max", "qwen-3.7-max", "qwen3.7-max-thinking"] {
        assert_rates(id, 2.50, 7.50, None, None);
    }
    for id in ["qwen3.7-plus", "qwen-3.7-plus", "qwen3.7-plus-preview"] {
        assert_rates(id, 0.40, 1.60, None, None);
    }
}

// ── Z.AI / GLM ──────────────────────────────────────────────────────────────

#[test]
fn glm_5_2_is_priced_and_ox_alpha_resolves_to_glm_5_3_flash() {
    // GLM-5.2 list: $1.40/$4.40, cached input $0.26. ox-alpha was the stealth
    // alias of GLM-5.3-Flash ($0.15/$0.50, cached read $0.03).
    assert_rates("glm-5.2", 1.40, 4.40, None, Some(0.26));
    assert_rates("ox-alpha", 0.15, 0.50, None, Some(0.03));
}

// ── Xiaomi MiMo ─────────────────────────────────────────────────────────────

#[test]
fn mimo_v2_6_is_priced_and_v2_5_stops_hitting_the_default() {
    // Overseas USD card: pro $0.435/$0.87, flash $0.14/$0.28.
    assert_rates("mimo-v2.6-pro", 0.435, 0.87, None, Some(0.0036));
    assert_rates("mimo-v2.6-flash", 0.14, 0.28, None, Some(0.0028));
    // The retired V2.5 ids cannot inherit V2.6 (version stripping folds both to
    // "mimo-v-pro" and the tie breaks to the dearer card), so 4.36B zero-cost
    // V2.5 tokens land on the V2 pro card instead of the $3/$15 default.
    assert_rates("mimo-v2.5-pro", 1.0, 3.0, None, None);
    assert_rates("mimo-v2.5-flash", 0.14, 0.28, None, Some(0.0028));
}

// ── Whole-id aliases ────────────────────────────────────────────────────────

#[test]
fn a_two_character_live_id_reaches_its_family_through_the_alias_table() {
    // Moonshot serves Kimi K3 as plain "k3". MIN_MATCH_OVERLAP is three, so the
    // id offers the "kimi-k3" row an overlap of exactly two and every substring
    // tier rejects it: the row existed and still never matched (#2254). The
    // alias table resolves it by whole id.
    assert_rates("k3", 3.0, 15.0, Some(3.75), Some(0.30));
    // And the alias must not swallow the longer ids that share the stem.
    assert_rates("k3-256k", 3.0, 15.0, None, Some(0.30));
    assert_rates("k3.1-pro", 4.0, 20.0, Some(4.0), Some(0.20));
    assert_rates("kimi-k3", 3.0, 15.0, None, None);
}

#[test]
fn the_gateway_placeholder_id_bills_the_unknown_model_default() {
    // "coder-model" is a placeholder written by six providers, not a model, so
    // the row deliberately carries the app's conservative unknown-model default
    // rather than an invented vendor card. It exists so the 34 legacy rows that
    // recorded zero stop reading as free.
    assert_rates("coder-model", 3.0, 15.0, Some(3.75), Some(0.30));
}
