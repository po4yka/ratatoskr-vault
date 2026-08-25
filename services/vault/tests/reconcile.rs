//! The reconciliation cycle end to end: one valid delivery through a fake source must enroll the
//! target, land its revision evidence, and emit the enrollment event.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault::reconcile::{DeliverySource, IncomingDelivery, run_cycle};
use ratatoskr_vault_core::delivery::DesiredStateDelivery;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

const CORR: &str = "018f3c1a-7b2c-7d4e-8f9a-000000000005";

/// An in-memory source serving exactly the messages it was built with.
struct FakeSource {
    pending: Vec<IncomingDelivery>,
}

impl DeliverySource for FakeSource {
    fn fetch_undelivered(&mut self) -> Vec<IncomingDelivery> {
        std::mem::take(&mut self.pending)
    }
}

/// The raw first policy a fresh target receives.
fn first_policy() -> DesiredStateDelivery {
    DesiredStateDelivery {
        preservation_level: "git_mirror".to_owned(),
        pinned: Some(false),
        include_wiki: Some(false),
        include_releases: Some(true),
        include_issues: Some(true),
        offsite_required: Some(true),
        correlation_id: CORR.to_owned(),
        policy_revision: Some(1),
    }
}

/// One valid delivery flows validate -> ingest -> converge -> emit: the target appears at its
/// machine's entry status and the enrollment event (null -> requested) owes the bus.
#[tokio::test]
async fn ingest_then_converge_applies_first_policy_end_to_end() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");

    let mut source = FakeSource {
        pending: vec![IncomingDelivery {
            provider: "github".to_owned(),
            external_repository_id: "cycle-first".to_owned(),
            source: "github-catalog".to_owned(),
            message_id: Uuid::now_v7(),
            delivery: first_policy(),
        }],
    };

    run_cycle(&fixture.database, &mut source)
        .await
        .expect("the first policy must flow end to end");

    let (status, target_id): (String, Uuid) = sqlx::query_as(
        "select status, target_id from git_vault.targets
         where provider = $1 and external_repository_id = $2",
    )
    .bind("github")
    .bind("cycle-first")
    .fetch_one(fixture.pool())
    .await
    .expect("the enrolled target must exist");
    assert_eq!(status, "requested");

    let events: Vec<(Option<String>, String, i64, String)> = sqlx::query_as(
        "select payload->>'from_status',
                payload->>'to_status',
                (payload->>'policy_revision')::bigint,
                payload->>'correlation_id'
         from git_vault.outbox
         where aggregate_id = $1 and event_type = 'vault.target.state_changed.v1'",
    )
    .bind(target_id)
    .fetch_all(fixture.pool())
    .await
    .expect("the outbox read must run");

    assert_eq!(events.len(), 1);
    assert!(
        events[0].0.is_none(),
        "enrollment emits null as the from_status"
    );
    assert_eq!(events[0].1, "requested");
    assert_eq!(events[0].2, 1);
    assert_eq!(events[0].3, CORR);

    fixture.cleanup().await.expect("cleanup");
}

/// How many events the bus is owed in total.
async fn outbox_total(fixture: &TestDatabase) -> i64 {
    sqlx::query_scalar("select count(*) from git_vault.outbox")
        .fetch_one(fixture.pool())
        .await
        .expect("the outbox count must run")
}

/// The stored status of the named repository's target.
async fn stored_status(fixture: &TestDatabase, external_id: &str) -> String {
    sqlx::query_scalar(
        "select status from git_vault.targets
         where provider = 'github' and external_repository_id = $1",
    )
    .bind(external_id)
    .fetch_one(fixture.pool())
    .await
    .expect("the status read must run")
}

fn replay_message() -> IncomingDelivery {
    IncomingDelivery {
        provider: "github".to_owned(),
        external_repository_id: "cycle-twice".to_owned(),
        source: "github-catalog".to_owned(),
        message_id: Uuid::now_v7(),
        delivery: first_policy(),
    }
}

/// Running the cycle twice over the same state changes nothing the second time: a replayed
/// `(source, message_id)` pair is absorbed silently, no new event owes the bus, the status
/// stays put, and the plan report shows zero executable work.
#[tokio::test]
async fn run_cycle_twice_is_fully_idempotent() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");

    // ONE message delivered twice: the exact transport-level replay the inbox exists for.
    let message = replay_message();

    let mut opener = FakeSource {
        pending: vec![message.clone()],
    };
    run_cycle(&fixture.database, &mut opener)
        .await
        .expect("the first cycle must run");

    let events_before = outbox_total(&fixture).await;
    let status_before = stored_status(&fixture, "cycle-twice").await;

    let mut replay = FakeSource {
        pending: vec![message],
    };
    let report = run_cycle(&fixture.database, &mut replay)
        .await
        .expect("a redelivered message must be absorbed silently");

    assert_eq!(report.executable, 0);
    assert_eq!(outbox_total(&fixture).await, events_before);
    assert_eq!(stored_status(&fixture, "cycle-twice").await, status_before);

    fixture.cleanup().await.expect("cleanup");
}
