//! Regression tests for #2161: a cron run must not strand itself on the
//! mechanical plan gate.
//!
//! `resolve_job_agent` (`src/cron/scheduler.rs`) returns the factory-built
//! agent for a job in the active profile. That early return meant the explicit
//! `with_headless(true)` and `with_plan_gate_exempt(true)` builder calls on the
//! foreign-profile branch below it never ran, and the factory carried no
//! plan-gate flag of its own. A job firing every two minutes died on call 13
//! asking for a plan nobody would ever approve.
//!
//! The fix moves both flags onto the factory through
//! [`configure_cron_factory`], so every agent the daemon's factory builds
//! carries them no matter which branch produced it.

use crate::brain::provider::Provider;
use crate::channels::{ChannelFactory, configure_cron_factory};
use crate::config::Config;
use crate::db::Database;
use crate::services::ServiceContext;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};

use super::agent_service_mocks::MockProvider;

async fn test_factory() -> ChannelFactory {
    let db = Database::connect_in_memory().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let (_, config_rx) = watch::channel(Config::default());
    ChannelFactory::new(
        Arc::new(MockProvider) as Arc<dyn Provider>,
        context,
        "test brain".to_string(),
        PathBuf::from("/tmp"),
        PathBuf::from("/tmp/oc_test_brain"),
        Arc::new(Mutex::new(None)),
        config_rx,
    )
}

/// The #2161 contract: a factory configured for cron execution yields agents
/// that are BOTH headless and plan-gate exempt.
///
/// Both are asserted in one test because the bug was exactly their divergence:
/// `headless` had a factory flag and `plan_gate_exempt` did not, so the
/// active-profile path inherited one and silently lost the other.
#[tokio::test]
async fn cron_plan_gate_factory_built_agent_is_exempt_and_headless() {
    let factory = test_factory().await;
    configure_cron_factory(&factory);

    let service = factory.create_agent_service_full(None, None).await;

    assert!(
        service.plan_gate_exempt(),
        "a cron-built agent must skip the mechanical plan gate (#2161)"
    );
    assert!(
        service.headless(),
        "a cron-built agent must stay headless (#129)"
    );
}

/// The same factory path the scheduler's queue variant takes, so the exemption
/// cannot be wired into one constructor and missed in the other.
#[tokio::test]
async fn cron_plan_gate_exempt_survives_the_queue_variant() {
    let factory = test_factory().await;
    configure_cron_factory(&factory);

    let service = factory.create_agent_service_with_queue(None).await;

    assert!(
        service.plan_gate_exempt(),
        "the queue variant delegates to the same builder and must inherit the exemption"
    );
}

/// Documents the interactive default: an unconfigured factory yields an agent
/// the gate applies to. Without this, the positive assertion above could pass
/// on a service that was exempt for some unrelated reason.
#[tokio::test]
async fn plan_gate_exempt_is_absent_by_default() {
    let factory = test_factory().await;

    let service = factory.create_agent_service_full(None, None).await;

    assert!(
        !service.plan_gate_exempt(),
        "an interactive channel factory must NOT be plan-gate exempt"
    );
    assert!(
        !service.headless(),
        "an interactive channel factory must NOT be headless"
    );
}

/// The flags are independent switches on the factory, not one packed value:
/// setting the exemption alone must not drag headless along, or a future
/// caller would get a silently headless surface.
#[tokio::test]
async fn plan_gate_exempt_does_not_imply_headless() {
    let factory = test_factory().await;
    factory.set_plan_gate_exempt(true);

    let service = factory.create_agent_service_full(None, None).await;

    assert!(service.plan_gate_exempt(), "the exemption was set directly");
    assert!(
        !service.headless(),
        "headless is a separate flag and must stay off"
    );
}
