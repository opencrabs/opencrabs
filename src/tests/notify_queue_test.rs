//! Notify-queue durability tests (#111).
//!
//! An undelivered push that parks in memory must survive a restart via its
//! DB row; the row must die on delivery — never before (lost push) and not
//! permanently after (duplicate spam). Content-exact clears are the law: a
//! blanket clear-for-session at a consume site could eat a never-delivered
//! mid-turn push for the same session.

use crate::brain::agent::{BgTaskMeta, PushOrigin};
use crate::db::Database;
use crate::db::repository::NotifyQueueRepository;
use uuid::Uuid;

/// One shared pool for the repo under test AND out-of-band row surgery the
/// public API deliberately does not offer (corruption, unknown-origin
/// fixtures).
async fn setup() -> (NotifyQueueRepository, Database) {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory database");
    db.run_migrations().await.expect("migrations");
    (NotifyQueueRepository::new(db.pool().clone()), db)
}

/// Run a raw SQL closure against the shared pool (the repo's own access
/// pattern, minus the repo).
async fn raw<F>(db: &Database, f: F)
where
    F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<usize> + Send + 'static,
{
    db.pool()
        .get()
        .await
        .expect("pool connection")
        .interact(move |conn| f(conn))
        .await
        .expect("interact")
        .expect("raw sql");
}

#[tokio::test]
async fn push_survives_as_row_until_cleared() {
    let (repo, _db) = setup().await;
    let id = Uuid::new_v4();
    let session = Uuid::new_v4();
    let meta = BgTaskMeta {
        task_id: "t-1".into(),
        success: true,
        label: "cargo test".into(),
        elapsed_secs: 12.0,
        tail: "test result: ok".into(),
    };

    repo.record(
        id,
        session,
        "context body",
        "display line",
        PushOrigin::BackgroundTask,
        Some(&meta),
    )
    .await
    .expect("record");

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].session_id, session);
    assert_eq!(rows[0].context_text, "context body");
    assert_eq!(rows[0].display_text, "display line");
    assert_eq!(rows[0].origin, PushOrigin::BackgroundTask);
    assert_eq!(
        rows[0].bg_meta.as_ref().map(|m| m.label.clone()),
        Some("cargo test".to_string())
    );

    // Delivery clears the row: it must not re-deliver after a restart.
    repo.clear(id).await.expect("clear");
    assert!(repo.all().await.expect("all").is_empty());
}

#[tokio::test]
async fn unknown_origin_row_maps_to_other() {
    let (repo, db) = setup().await;
    let session = Uuid::new_v4();

    // A row carrying an origin the current code does not know (a future
    // version wrote it). The reader maps it to Other, never fails.
    raw(&db, move |conn| {
        conn.execute(
            "INSERT INTO notify_queue \
             (id, session_id, context_text, display_text, origin, bg_meta, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'origin_from_the_future', NULL, 0)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                session.to_string(),
                "ctx",
                "disp"
            ],
        )
    })
    .await;

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, PushOrigin::Other);
    assert!(rows[0].bg_meta.is_none());
}

#[tokio::test]
async fn clear_matching_spares_different_content_same_session() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();

    repo.record(
        Uuid::new_v4(),
        session,
        "delivered push",
        "delivered push",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record delivered");

    repo.record(
        Uuid::new_v4(),
        session,
        "never delivered push",
        "never delivered push",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record undelivered");

    // The delivered twin goes out; the content-exact clear must touch ONLY
    // its row — the never-delivered push for the same session survives.
    repo.clear_matching(session, "delivered push", "delivered push")
        .await
        .expect("clear_matching");

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].context_text, "never delivered push");
}

#[tokio::test]
async fn clear_for_session_clears_all_for_that_session_only() {
    let (repo, _db) = setup().await;
    let (session_a, session_b) = (Uuid::new_v4(), Uuid::new_v4());

    for (session, text) in [(session_a, "a"), (session_a, "a2"), (session_b, "b")] {
        repo.record(Uuid::new_v4(), session, text, text, PushOrigin::Other, None)
            .await
            .expect("record");
    }

    repo.clear_for_session(session_a).await.expect("clear");

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, session_b);
}

#[tokio::test]
async fn corrupt_bg_meta_row_is_skipped_not_fatal() {
    let (repo, db) = setup().await;
    let session = Uuid::new_v4();

    // Corrupt JSON in bg_meta: the reader logs, skips the field, and the
    // row still comes back with bg_meta=None — redelivery proceeds.
    raw(&db, move |conn| {
        conn.execute(
            "INSERT INTO notify_queue \
             (id, session_id, context_text, display_text, origin, bg_meta, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'session_notify', '{not-json', 0)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                session.to_string(),
                "ctx",
                "disp"
            ],
        )
    })
    .await;

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, PushOrigin::SessionNotify);
    assert!(rows[0].bg_meta.is_none());
}

#[tokio::test]
async fn clear_dead_sessions_reaps_only_rows_for_missing_sessions() {
    let (repo, db) = setup().await;
    let live = Uuid::new_v4();
    let dead = Uuid::new_v4();

    // A session row that still exists (sessions.id is TEXT). `raw` runs the
    // statement against the shared pool; the repo API offers no session write.
    raw(&db, move |conn| {
        conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at) VALUES (?1, 0, 0)",
            rusqlite::params![live.to_string()],
        )
    })
    .await;

    repo.record(
        Uuid::new_v4(),
        live,
        "live ctx",
        "live disp",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record live");
    repo.record(
        Uuid::new_v4(),
        dead,
        "dead ctx",
        "dead disp",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record dead");

    let reaped = repo.clear_dead_sessions().await.expect("reap");
    assert_eq!(
        reaped.len(),
        1,
        "only the row whose session is gone is dropped"
    );
    assert_eq!(
        reaped[0].session_id, dead,
        "reaper returns the dead row for auditing"
    );
    assert_eq!(reaped[0].origin, PushOrigin::SessionNotify);

    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1, "the live-session row survives");
    assert_eq!(rows[0].session_id, live);
}

#[tokio::test]
async fn reap_stale_unclaimed_deletes_only_rows_past_age_cutoff() {
    let (repo, db) = setup().await;
    let now = 1_700_000_000i64;
    let session = Uuid::new_v4();
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let id_c = Uuid::new_v4();

    // Insert rows with explicit created_at timestamps via raw SQL:
    // Row A: 10h old
    // Row B: 25h old (past 24h warning threshold, under 72h ceiling)
    // Row C: 80h old (past 72h ceiling)
    for (id, ts, text) in [
        (id_a, now - 10 * 3600, "10h old"),
        (id_b, now - 25 * 3600, "25h old"),
        (id_c, now - 80 * 3600, "80h old"),
    ] {
        let sess_str = session.to_string();
        raw(&db, move |conn| {
            conn.execute(
                "INSERT INTO notify_queue \
                 (id, session_id, context_text, display_text, origin, bg_meta, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'session_notify', NULL, ?5)",
                rusqlite::params![id.to_string(), sess_str, text, text, ts],
            )
        })
        .await;
    }

    let cutoff = now - 72 * 3600;
    let reaped = repo.reap_stale_unclaimed(cutoff).await.expect("reap");

    // Exactly Row C reaped
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].id, id_c);
    assert_eq!(reaped[0].session_id, session);
    assert_eq!(reaped[0].context_text, "80h old");
    assert_eq!(reaped[0].origin, PushOrigin::SessionNotify);

    // Rows A and B survive in DB
    let remaining = repo.all().await.expect("all");
    assert_eq!(remaining.len(), 2);
    let remaining_ids: Vec<Uuid> = remaining.into_iter().map(|r| r.id).collect();
    assert!(remaining_ids.contains(&id_a));
    assert!(remaining_ids.contains(&id_b));
    assert!(!remaining_ids.contains(&id_c));
}

#[tokio::test]
async fn reap_stale_unclaimed_handles_empty_table_and_boundary_cases() {
    let (repo, db) = setup().await;
    let cutoff = 1_700_000_000i64;

    // 1. Empty table returns Ok([]) without error
    let reaped_empty = repo.reap_stale_unclaimed(cutoff).await.expect("reap empty");
    assert!(reaped_empty.is_empty());

    // 2. Exact boundary check: created_at == cutoff survives, cutoff - 1 is reaped
    let session = Uuid::new_v4();
    let id_at_boundary = Uuid::new_v4();
    let id_below_boundary = Uuid::new_v4();

    for (id, ts, text) in [
        (id_at_boundary, cutoff, "at boundary"),
        (id_below_boundary, cutoff - 1, "below boundary"),
    ] {
        let sess_str = session.to_string();
        raw(&db, move |conn| {
            conn.execute(
                "INSERT INTO notify_queue \
                 (id, session_id, context_text, display_text, origin, bg_meta, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'background_task', NULL, ?5)",
                rusqlite::params![id.to_string(), sess_str, text, text, ts],
            )
        })
        .await;
    }

    let reaped = repo
        .reap_stale_unclaimed(cutoff)
        .await
        .expect("reap boundary");
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].id, id_below_boundary);
    assert_eq!(reaped[0].origin, PushOrigin::BackgroundTask);

    let remaining = repo.all().await.expect("all");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, id_at_boundary);
}

// ---------------------------------------------------------------------------
// #439 / #366 — the durable twin must not survive the delivery that wrote it.
//
// Measured defect (2026-09-30): a delivery re-persisted the push it was
// delivering, while the dispatcher's content-exact clear had already fired
// with the PRE-framing text and so could not match the framed row. The row
// survived, the next boot re-delivered it, and the busy-lane framing stacked
// one wrapper per boot — two live rows read `wrappers=2`.
//
// The invariant these pin: the durable row carries the push's OWN text, and
// the framing is applied to the in-memory item only. That is what lets the
// clear match the row its own delivery wrote.
// ---------------------------------------------------------------------------

use crate::brain::agent::service::notify_queue::{
    BUSY_LANE_WRAPPER, durable_context_text, strip_busy_wrapper, wrap_busy_once,
};

#[test]
fn busy_framing_never_reaches_the_durable_row() {
    let body = "session-notify from=lane-x\n\nbody text";
    let in_memory = wrap_busy_once(body);
    assert_ne!(in_memory, body, "the in-memory item carries the framing");

    let stored = durable_context_text(&in_memory);
    assert_eq!(stored, body, "the durable row carries the push's own text");
    assert!(!stored.contains(BUSY_LANE_WRAPPER));
}

#[test]
fn repeated_busy_framing_is_stripped_to_none() {
    // Rows written before the framing was excluded from the durable text can
    // carry two or more (measured live: `wrappers=2`). A single-pass strip
    // would leave one behind, and the content-exact match would still miss.
    let stacked = format!("{BUSY_LANE_WRAPPER}{BUSY_LANE_WRAPPER}payload");
    assert_eq!(strip_busy_wrapper(&stacked), "payload");
    assert_eq!(strip_busy_wrapper("payload"), "payload");
    assert_eq!(strip_busy_wrapper(""), "");
}

#[test]
fn busy_framing_is_applied_at_most_once() {
    let once = wrap_busy_once("payload");
    assert_eq!(once, format!("{BUSY_LANE_WRAPPER}payload"));
    assert_eq!(
        wrap_busy_once(&once),
        once,
        "a re-offer must not stack a second wrapper"
    );
    assert_eq!(wrap_busy_once(&format!("{BUSY_LANE_WRAPPER}{once}")), once);
}

#[tokio::test]
async fn a_delivery_retires_the_row_its_own_write_produced() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();
    let (body, display) = ("the push body", "the push body");
    let in_memory = wrap_busy_once(body); // what the turn hands around

    // `persist` writes the normalised text.
    let stored = durable_context_text(&in_memory);
    repo.record(
        Uuid::new_v4(),
        session,
        &stored,
        display,
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record");

    // `clear_on_delivery` clears with the normalised text.
    repo.clear_matching(session, &durable_context_text(&in_memory), display)
        .await
        .expect("clear_matching");
    assert!(
        repo.all().await.expect("all").is_empty(),
        "the delivery must retire the row its own write produced"
    );

    // Counterfactual — the pre-fix behaviour this exists to prevent. Clearing
    // with the FRAMED text (what the in-memory item carries, and what every
    // clear site passed before the normalisation) does not match the row, so
    // the row survived its own delivery: the boot re-delivery loop.
    repo.record(
        Uuid::new_v4(),
        session,
        &stored,
        display,
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record");
    repo.clear_matching(session, &in_memory, display)
        .await
        .expect("clear_matching framed");
    assert_eq!(
        repo.all().await.expect("all").len(),
        1,
        "pre-fix: the framed clear misses the row — the defect this pins"
    );
}

// ---------------------------------------------------------------------------
// Ordering — the clear must not precede the write it retires (#439/#366,
// shape A).
//
// `route` is `Arc<dyn Fn(Uuid, QueuedUserMessage)>` returning `()`: the
// delivery — and therefore its `enqueue_item` → `persist` INSERT — is deferred
// into a spawn behind a transport wait. A clear issued at the same chokepoint
// reaches the pool FIRST and retires nothing, and the row the delivery then
// writes survives to the next boot: one byte-identical duplicate per boot,
// forever. These tests pin the wait that closes it.
// ---------------------------------------------------------------------------

use crate::brain::agent::service::notify_queue::retire_rows_until;

/// The discriminating leg: a persist that lands AFTER the clear has started
/// must still be retired.
#[tokio::test]
async fn clear_waits_for_the_write_it_must_retire() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();
    let (body, display) = ("a work order body", "work order");

    // The row the push parked at its first hand-off.
    repo.record(
        Uuid::new_v4(),
        session,
        body,
        display,
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record original");

    // The delivering route's OWN write, deferred exactly as the spawned
    // callback defers it — landing 600ms after the clear begins.
    let late = repo.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        late.record(
            Uuid::new_v4(),
            session,
            body,
            display,
            PushOrigin::SessionNotify,
            None,
        )
        .await
        .expect("record deferred");
    });

    let retired = retire_rows_until(
        &repo,
        session,
        body,
        display,
        std::time::Duration::from_secs(3),
        std::time::Duration::from_millis(250),
    )
    .await
    .expect("retire");

    assert_eq!(
        retired, 2,
        "the parked row AND the deferred write must both be retired"
    );
    assert!(
        repo.all().await.expect("all").is_empty(),
        "a row surviving this is the duplicate the next boot re-delivers"
    );
}

/// Pre-fix rows carry the busy-lane framing (the normalisation landed after
/// them), so an exact-content match can never retire them: they were immortal
/// until the 72h reaper.
#[tokio::test]
async fn clear_retires_a_pre_fix_row_that_still_carries_the_framing() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();
    let (body, display) = ("a durable twin body", "twin");
    let framed = format!("{BUSY_LANE_WRAPPER}{body}");

    repo.record(
        Uuid::new_v4(),
        session,
        &framed,
        display,
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record framed");

    let retired = retire_rows_until(
        &repo,
        session,
        body,
        display,
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(25),
    )
    .await
    .expect("retire");

    assert_eq!(
        retired, 1,
        "the framing must not shield a row from its own delivery"
    );
    assert!(repo.all().await.expect("all").is_empty());
}

/// Positive control for the match: a DIFFERENT undelivered push for the same
/// session must survive, or the clear is a blanket session wipe.
#[tokio::test]
async fn clear_leaves_a_different_undelivered_push_alone() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();
    repo.record(
        Uuid::new_v4(),
        session,
        "delivered",
        "delivered",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record delivered");
    repo.record(
        Uuid::new_v4(),
        session,
        "never delivered",
        "never delivered",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record pending");

    let retired = retire_rows_until(
        &repo,
        session,
        "delivered",
        "delivered",
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(25),
    )
    .await
    .expect("retire");

    assert_eq!(retired, 1);
    let rows = repo.all().await.expect("all");
    assert_eq!(rows.len(), 1, "the never-delivered push must survive");
    assert_eq!(rows[0].context_text, "never delivered");
}

/// Negative control: retiring nothing reports zero and deletes nothing — the
/// instrument can observe the absence it is aimed at.
#[tokio::test]
async fn clear_of_an_absent_row_reports_zero_and_deletes_nothing() {
    let (repo, _db) = setup().await;
    let session = Uuid::new_v4();
    repo.record(
        Uuid::new_v4(),
        session,
        "unrelated",
        "unrelated",
        PushOrigin::SessionNotify,
        None,
    )
    .await
    .expect("record");

    let retired = retire_rows_until(
        &repo,
        session,
        "no such body",
        "no such display",
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(25),
    )
    .await
    .expect("retire");

    assert_eq!(retired, 0, "nothing matched, so nothing may be deleted");
    assert_eq!(repo.all().await.expect("all").len(), 1);
}
