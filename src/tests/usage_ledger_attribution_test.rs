//! Usage is attributed to the pair that served it (#807).
//!
//! The ledger used to re-derive provider and model from the session row rather
//! than record what actually served. Two consequences, both permanent because
//! the ledger is append-only:
//!
//! A row left inconsistent by a sticky fallback or a partial switch (#705) was
//! copied verbatim, minting a record of a request that cannot exist. Real
//! example from a live database: a `xiaomi` row naming `glm-5.1`, a model
//! Xiaomi does not serve. And when a fallback served the turn, its spend was
//! credited to the primary that did not.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::db::Database;
use crate::db::repository::UsageLedgerRepository;
use crate::services::{ServiceContext, SessionService};

async fn service_and_pool() -> (SessionService, crate::db::Pool) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();
    (SessionService::new(ServiceContext::new(pool.clone())), pool)
}

#[tokio::test]
async fn usage_records_the_serving_pair_not_the_session_row() {
    let (service, pool) = service_and_pool().await;
    let mut session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    // The row claims one pair. This is the inconsistent state #705 produces,
    // and the exact shape that used to be copied into the ledger.
    session.provider_name = Some("xiaomi".to_string());
    session.model = Some("glm-5.1".to_string());
    service.update_session(&session).await.unwrap();

    // A different provider actually served the turn.
    service
        .update_session_usage(session.id, 100, 0.05, "zhipu", "glm-5.1", None, None)
        .await
        .unwrap();

    let ledger = UsageLedgerRepository::new(pool);
    let rows = ledger.by_provider_model(None, None, None).await.unwrap();
    assert_eq!(rows.len(), 1, "expected one usage row, got: {rows:?}");
    assert_eq!(
        rows[0].0, "zhipu",
        "cost must be attributed to the provider that served it, not the session's stored one"
    );
    assert!(
        !rows.iter().any(|r| r.0 == "xiaomi"),
        "the impossible xiaomi/glm pair must never be recorded: {rows:?}"
    );
}

#[tokio::test]
async fn the_session_row_is_not_rewritten_by_recording_usage() {
    // Recording usage accounts for spend; it does not decide which provider the
    // session belongs to. Overwriting the row here would turn a one-off
    // fallback into a permanent switch the user never made (#705).
    let (service, _pool) = service_and_pool().await;
    let mut session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();
    session.provider_name = Some("xiaomi".to_string());
    session.model = Some("mimo-v2.5-pro".to_string());
    service.update_session(&session).await.unwrap();

    service
        .update_session_usage(session.id, 100, 0.05, "zhipu", "glm-5.1", None, None)
        .await
        .unwrap();

    let after = service.get_session_required(session.id).await.unwrap();
    assert_eq!(after.provider_name.as_deref(), Some("xiaomi"));
    assert_eq!(after.model.as_deref(), Some("mimo-v2.5-pro"));
}

#[tokio::test]
async fn token_and_cost_totals_still_accumulate() {
    let (service, _pool) = service_and_pool().await;
    let session = service
        .create_session(Some("Test".to_string()))
        .await
        .unwrap();

    service
        .update_session_usage(session.id, 100, 0.05, "xiaomi", "mimo-v2.5-pro", None, None)
        .await
        .unwrap();
    service
        .update_session_usage(session.id, 50, 0.025, "xiaomi", "mimo-v2.5-pro", None, None)
        .await
        .unwrap();

    let updated = service.get_session_required(session.id).await.unwrap();
    assert_eq!(updated.token_count, 150);
    assert!((updated.total_cost - 0.075).abs() < 0.0001);
}
