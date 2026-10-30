//! A response with no cache field is \"not reported\", not a zero cache read
//! (#2139). A response that reports zero is zero, and is recorded as zero.

use crate::brain::provider::custom_openai_compatible::OpenAIUsage;

#[test]
fn a_response_with_no_cache_field_is_not_reported() {
    let usage: OpenAIUsage =
        serde_json::from_str(r#"{"prompt_tokens": 1000, "completion_tokens": 50}"#)
            .expect("usage parses");
    assert!(
        usage.cache_read_absent(),
        "no cache field means not reported"
    );
    assert_eq!(usage.effective_cache_read(), 0);
}

#[test]
fn a_response_that_reports_zero_is_reported_as_zero() {
    let usage: OpenAIUsage = serde_json::from_str(
        r#"{"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 0}}"#,
    )
    .expect("usage parses");
    assert!(
        !usage.cache_read_absent(),
        "a stated zero is data, not absence"
    );
    assert_eq!(usage.effective_cache_read(), 0);
}

#[test]
fn deepseek_hits_count_as_reported() {
    let usage: OpenAIUsage = serde_json::from_str(
        r#"{"prompt_tokens": 1000, "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100}"#,
    )
    .expect("usage parses");
    assert!(!usage.cache_read_absent());
}

#[test]
fn a_provider_usage_defaults_to_reported() {
    // Every provider that does not set the flag keeps recording its value.
    let usage = crate::brain::provider::types::TokenUsage::default();
    assert!(!usage.cache_read_unreported);
}

#[test]
fn a_top_level_cached_tokens_field_is_read_when_nothing_else_is_present() {
    // Moonshot-style usage: cached_tokens at the top of the usage object (#2140).
    let usage: OpenAIUsage =
        serde_json::from_str(r#"{"prompt_tokens": 1000, "cached_tokens": 700}"#)
            .expect("usage parses");
    assert_eq!(usage.effective_cache_read(), 700);
    assert!(!usage.cache_read_absent());
}
