//! The By Model cache share divides cache reads by prompt tokens, and DeepSeek's
//! own cache-hit field is read (#2136).

use crate::brain::provider::custom_openai_compatible::OpenAIUsage;
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use crate::usage::data::{DashboardData, Period};

#[test]
fn deepseek_cache_hits_are_read_from_their_own_field() {
    // DeepSeek reports hits as prompt_cache_hit_tokens, with the uncached part
    // as prompt_cache_miss_tokens. The hits must count as cache reads.
    let json = r#"{"prompt_tokens": 1000, "completion_tokens": 50,
        "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100}"#;
    let usage: OpenAIUsage = serde_json::from_str(json).expect("usage parses");
    assert_eq!(usage.effective_cache_read(), 900);
}

#[test]
fn an_openai_cache_field_still_wins_when_present() {
    let json = r#"{"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 400}}"#;
    let usage: OpenAIUsage = serde_json::from_str(json).expect("usage parses");
    assert_eq!(usage.effective_cache_read(), 400);
}

#[tokio::test]
async fn the_share_divides_by_prompt_tokens_not_by_the_row_total() {
    // 1,500 tokens in the row: 1,000 prompt (900 served from cache) and 500 of
    // output. The share is 900 of 1,000 prompt tokens, 90%, not 900 of 1,500.
    let home = tempfile::tempdir().expect("tempdir");
    let models =
        crate::config::profile::with_home_override_async(home.path().to_path_buf(), async move {
            let db = Database::connect_in_memory().await.expect("in-memory DB");
            db.run_migrations().await.expect("migrations");
            let service = SessionService::new(ServiceContext::new(db.pool().clone()));
            let session = service
                .create_session(Some("prompt share".to_string()))
                .await
                .expect("session");
            service
                .update_session_usage(
                    session.id,
                    1_500,
                    0.01,
                    "opencrabs",
                    "deepseek-v4.1-flash",
                    Some(900),
                    Some(1_000),
                )
                .await
                .expect("record usage");
            DashboardData::fetch(db.pool(), Period::AllTime)
                .await
                .expect("dashboard fetch")
                .models
        })
        .await;
    let m = models
        .iter()
        .find(|m| m.model.contains("deepseek"))
        .expect("deepseek row");
    let pct = m.cache_read_pct().expect("a recorded split");
    assert!(
        (pct - 90.0).abs() < 0.05,
        "900 of 1,000 prompt tokens is 90%, got {pct}"
    );
}
