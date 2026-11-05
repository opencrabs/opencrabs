//! Tests for the additive baseline-merge mechanisms.
//!
//! Two surfaces shipped this release:
//!
//! 1. **Pricing additive merge** — `PricingConfig::load` merges any
//!    `(provider, prefix)` from the bundled `usage_pricing.toml.example`
//!    that's missing from the user's live `~/.opencrabs/usage_pricing.toml`,
//!    then writes the merged file back to disk. Without this the
//!    seed-on-missing pattern froze the user's pricing table at
//!    whatever shipped on their first install — MiniMax-M3 (or any
//!    future model) would never reach existing users via a binary
//!    upgrade.
//!
//! 2. **MiniMax model-list runtime merge** — `fetch_provider_models`
//!    on the `minimax` branch returns
//!    `merge_minimax_baseline(baseline, user_config)` instead of
//!    just `user_config.models`. New baseline entries (MiniMax-M3
//!    today, MiniMax-M4 tomorrow) land at the top of the picker on
//!    every binary upgrade, but the user's custom additions (private
//!    variants, MiniMax-Text-01, etc.) are preserved at the end.
//!
//! Both are strictly additive: user customisations never disappear,
//! and re-running is idempotent (zero entries added on the second
//! call with the same baseline).

use crate::tui::onboarding::{merge_minimax_baseline, xiaomi_baseline_models};
use crate::usage::pricing::{PricingConfig, PricingEntry, ProviderBlock};
use std::collections::HashMap;

// ── Pricing additive merge ─────────────────────────────────────

fn pricing_with(entries: Vec<(&str, f64, f64)>) -> PricingConfig {
    let mut providers = HashMap::new();
    let block = ProviderBlock {
        entries: entries
            .into_iter()
            .map(|(prefix, i, o)| PricingEntry {
                prefix: prefix.to_string(),
                input_per_m: i,
                output_per_m: o,
                cache_write_per_m: None,
                cache_read_per_m: None,
            })
            .collect(),
    };
    providers.insert("minimax".to_string(), block);
    PricingConfig { providers }
}

#[test]
fn pricing_merge_appends_missing_entries() {
    // User has the old set; baseline adds MiniMax-M3. After merge
    // the user should have M3 at the end of their block (additive
    // — never reorders the user's entries).
    let mut user = pricing_with(vec![
        ("minimax-m2.7", 0.30, 1.20),
        ("minimax-m2.5", 0.30, 1.20),
    ]);
    let baseline = pricing_with(vec![
        ("minimax-m3", 0.60, 2.40),
        ("minimax-m2.7", 0.30, 1.20),
        ("minimax-m2.5", 0.30, 1.20),
    ]);
    let added = user.merge_missing_from(&baseline);
    assert_eq!(
        added, 1,
        "exactly one new entry (minimax-m3) should be appended"
    );
    let prefixes: Vec<&str> = user.providers["minimax"]
        .entries
        .iter()
        .map(|e| e.prefix.as_str())
        .collect();
    assert!(prefixes.contains(&"minimax-m3"), "M3 must be appended");
    assert!(prefixes.contains(&"minimax-m2.7"), "user's M2.7 preserved");
    assert!(prefixes.contains(&"minimax-m2.5"), "user's M2.5 preserved");
}

#[test]
fn pricing_merge_is_case_insensitive_on_prefix() {
    // Some users wrote `MiniMax-M2.7` capitalised, the baseline uses
    // lowercase `minimax-m2.7`. The merge must not double-add.
    let mut user = pricing_with(vec![("MiniMax-M2.7", 0.30, 1.20)]);
    let baseline = pricing_with(vec![("minimax-m2.7", 0.30, 1.20)]);
    let added = user.merge_missing_from(&baseline);
    assert_eq!(
        added, 0,
        "case-different but same prefix must not duplicate"
    );
}

#[test]
fn pricing_merge_is_idempotent() {
    // Running the merge twice with the same baseline should add the
    // entries once on the first call and zero on the second.
    let mut user = pricing_with(vec![("minimax-m2.7", 0.30, 1.20)]);
    let baseline = pricing_with(vec![
        ("minimax-m3", 0.60, 2.40),
        ("minimax-m2.7", 0.30, 1.20),
    ]);
    let first = user.merge_missing_from(&baseline);
    assert_eq!(first, 1, "first call appends M3");
    let second = user.merge_missing_from(&baseline);
    assert_eq!(second, 0, "second call adds zero (idempotent)");
}

#[test]
fn pricing_merge_handles_new_provider_block() {
    // User has only `[providers.minimax]`; baseline adds entries
    // under a new `[providers.zhipu]` block the user doesn't have
    // yet. The merge must create the new block, not skip it.
    let mut user = pricing_with(vec![("minimax-m2.7", 0.30, 1.20)]);
    let mut baseline_providers = HashMap::new();
    baseline_providers.insert(
        "zhipu".to_string(),
        ProviderBlock {
            entries: vec![PricingEntry {
                prefix: "glm-5.1".to_string(),
                input_per_m: 0.50,
                output_per_m: 2.00,
                cache_write_per_m: None,
                cache_read_per_m: None,
            }],
        },
    );
    let baseline = PricingConfig {
        providers: baseline_providers,
    };
    let added = user.merge_missing_from(&baseline);
    assert_eq!(added, 1, "new-provider block must contribute its entries");
    assert!(user.providers.contains_key("zhipu"));
    assert_eq!(user.providers["zhipu"].entries.len(), 1);
}

#[test]
fn pricing_merge_never_overwrites_user_rates() {
    // User has manually set `minimax-m2.7` to a private discounted
    // rate (input 0.10 instead of the baseline 0.30). The merge
    // must NOT replace that — `minimax-m2.7` already exists in
    // user's list, so the baseline's entry is skipped.
    let mut user = pricing_with(vec![("minimax-m2.7", 0.10, 0.40)]);
    let baseline = pricing_with(vec![("minimax-m2.7", 0.30, 1.20)]);
    let added = user.merge_missing_from(&baseline);
    assert_eq!(
        added, 0,
        "existing prefix means baseline is skipped entirely"
    );
    let entry = &user.providers["minimax"].entries[0];
    assert_eq!(entry.input_per_m, 0.10, "user's rate preserved");
    assert_eq!(entry.output_per_m, 0.40, "user's rate preserved");
}

// ── MiniMax model-list runtime merge ───────────────────────────

#[test]
fn minimax_merge_puts_baseline_first_user_last() {
    let baseline = vec![
        "MiniMax-M3".to_string(),
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.5".to_string(),
        "MiniMax-M2.1".to_string(),
    ];
    let user = vec![
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.5".to_string(),
        "MiniMax-M2.1".to_string(),
        "MiniMax-Text-01".to_string(),
    ];
    let merged = merge_minimax_baseline(baseline, user);
    assert_eq!(
        merged.len(),
        5,
        "5 distinct entries — 4 baseline + 1 user-only"
    );
    assert_eq!(
        merged[0], "MiniMax-M3",
        "baseline first (newest at top of picker)"
    );
    assert_eq!(
        merged[4], "MiniMax-Text-01",
        "user-only entries appended at the end"
    );
}

#[test]
fn minimax_merge_case_insensitive_dedup() {
    // User wrote `minimax-m3` lowercase, baseline has `MiniMax-M3`.
    // The merge keeps the baseline version (first wins on case-
    // insensitive comparison) and skips the user's duplicate.
    let baseline = vec!["MiniMax-M3".to_string()];
    let user = vec!["minimax-m3".to_string(), "MiniMax-Text-01".to_string()];
    let merged = merge_minimax_baseline(baseline, user);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0], "MiniMax-M3");
    assert_eq!(merged[1], "MiniMax-Text-01");
}

#[test]
fn minimax_merge_empty_user_returns_baseline_only() {
    let baseline = vec!["MiniMax-M3".to_string(), "MiniMax-M2.7".to_string()];
    let merged = merge_minimax_baseline(baseline.clone(), Vec::new());
    assert_eq!(merged, baseline);
}

#[test]
fn minimax_merge_empty_baseline_returns_user_only() {
    // Defensive: if a future refactor accidentally empties the
    // baseline, the user's saved list still survives.
    let user = vec!["MiniMax-M2.7".to_string()];
    let merged = merge_minimax_baseline(Vec::new(), user.clone());
    assert_eq!(merged, user);
}

#[test]
fn minimax_merge_internal_user_dedup() {
    // User's config has the same entry twice (rare, but possible
    // from a botched manual edit). The merge dedups internally.
    let baseline = vec!["MiniMax-M3".to_string()];
    let user = vec![
        "MiniMax-M2.7".to_string(),
        "MiniMax-M2.7".to_string(),
        "MiniMax-Text-01".to_string(),
    ];
    let merged = merge_minimax_baseline(baseline, user);
    assert_eq!(merged.len(), 3, "duplicates removed inside user list too");
}

// ── Xiaomi baseline fallback (#1419) ──────────────────────────
//
// The xiaomi branch GETs a keyless live endpoint that answers 401 on
// some CI runners. Before #1419 an empty fetch returned an empty list,
// so the onboarding picker showed nothing and the /models test red-lined
// the whole Test job. The fix mirrors MiniMax: fall back to a
// compiled-in baseline. The branch shadows the base_url parameter
// (fetch.rs:341), so it cannot be driven offline through a refused
// local port — these pin the baseline data directly and assert the
// branch is still wired to it.

#[test]
fn xiaomi_baseline_is_never_empty_and_every_entry_is_a_mimo_model() {
    // The picker's whole guarantee after a failed fetch. The /models
    // test in xiaomi_onboarding_test.rs asserts the same two properties,
    // but only by way of the live endpoint; this needs no network.
    let baseline = xiaomi_baseline_models();
    assert!(!baseline.is_empty(), "the fallback must never be empty");
    for model in &baseline {
        assert!(
            model.contains("mimo"),
            "every baseline entry must be a mimo model, got {model}"
        );
    }
}

#[test]
fn xiaomi_baseline_has_no_duplicates() {
    // merge_minimax_baseline dedups the user list against the baseline,
    // not the baseline against itself, so a repeated entry here would
    // reach the picker twice.
    let baseline = xiaomi_baseline_models();
    let mut seen: Vec<&String> = Vec::new();
    for model in &baseline {
        assert!(
            !seen.iter().any(|s| s.eq_ignore_ascii_case(model)),
            "baseline lists {model} twice"
        );
        seen.push(model);
    }
}

#[test]
fn xiaomi_fallback_keeps_user_models_and_stays_non_empty() {
    // Exactly the shape the fix returns when the live fetch comes back
    // empty: baseline first, saved models preserved, and still non-empty
    // with no user config at all.
    let user = vec!["mimo-custom-variant".to_string()];
    let merged = merge_minimax_baseline(xiaomi_baseline_models(), user.clone());
    assert!(merged.len() > user.len(), "the baseline must add entries");
    for model in &user {
        assert!(merged.contains(model), "user model {model} must survive");
    }
    let alone = merge_minimax_baseline(xiaomi_baseline_models(), Vec::new());
    assert!(
        !alone.is_empty(),
        "no user config must still yield a usable picker"
    );
}

#[test]
fn xiaomi_branch_falls_back_to_the_baseline_when_the_fetch_is_empty() {
    // The regression trip. The three tests above are worthless if the
    // branch stops consulting the baseline, and that absence is exactly
    // the bug: no `is_empty` guard, straight to merge(api_models, user).
    // A source sentinel because the branch cannot be driven offline.
    //
    // Line-based, and deliberately NOT sliced on a structural anchor:
    // `let client = reqwest::Client::new()` appears inside the xiaomi
    // branch as well as after it, so splitting on it truncates the
    // branch before the fallback. Locate the unique call instead and
    // read the lines above it, which keeps the window tight enough that
    // a fallback wired to some other condition still fails.
    let src = include_str!("../ui_core/model_selector/fetch.rs");
    let call = "merge_minimax_baseline(xiaomi_baseline_models(), user_models)";
    let lines: Vec<&str> = src.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(call))
        .expect("the empty-fetch fallback must merge the compiled-in xiaomi baseline");
    // Inclusive of the call line: `return` is on it, not above it.
    let window = lines[at.saturating_sub(12)..=at].join("\n");
    assert!(
        window.contains("if api_models.is_empty()"),
        "an empty live fetch must take a fallback path, not merge nothing"
    );
    assert!(
        window.contains("return"),
        "the empty-fetch branch must return the baseline, not fall through to merge the empty list"
    );
}
