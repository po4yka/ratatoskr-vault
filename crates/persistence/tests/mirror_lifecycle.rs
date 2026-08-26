//! Durable quota admission and cancellation evidence against a disposable `PostgreSQL` database.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::error::FailureClass;
use ratatoskr_vault_core::mirror::{MirrorCheckpoint, MirrorOperation, MirrorResult};
use ratatoskr_vault_persistence::QuotaReservationOutcome;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

async fn fixture_target(fixture: &TestDatabase) -> Uuid {
    let target_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'requested', now(), now())",
    )
    .bind(target_id)
    .bind(Uuid::now_v7().simple().to_string())
    .execute(fixture.pool())
    .await
    .expect("fixture target inserts");
    target_id
}

#[tokio::test]
async fn quota_reservation_refuses_global_overcommit_without_mutating_existing_reservation() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let first = fixture_target(&fixture).await;
    let second = fixture_target(&fixture).await;
    let first_run = Uuid::now_v7();
    let second_run = Uuid::now_v7();

    let admitted = fixture
        .database
        .reserve_mirror_quota(first, first_run, 70, 100, 100)
        .await
        .expect("first reservation query succeeds");
    assert_eq!(admitted, QuotaReservationOutcome::Reserved);

    let refused = fixture
        .database
        .reserve_mirror_quota(second, second_run, 40, 100, 100)
        .await
        .expect("overquota is a durable domain answer");
    assert_eq!(
        refused,
        QuotaReservationOutcome::QuotaExceeded,
        "70 reserved + 40 requested exceeds the 100-byte global budget"
    );

    let reservations: Vec<(Uuid, Uuid, i64)> = sqlx::query_as(
        "select target_id, run_id, reserved_bytes from git_vault.mirror_quota_reservations
         order by target_id",
    )
    .fetch_all(fixture.pool())
    .await
    .expect("the first reservation remains readable");
    assert_eq!(reservations, vec![(first, first_run, 70)]);

    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn interrupted_run_releases_reservation_and_keeps_checkpoint() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let target = fixture_target(&fixture).await;
    let run_id = Uuid::now_v7();

    fixture
        .database
        .reserve_mirror_quota(target, run_id, 10, 100, 100)
        .await
        .expect("reservation query");
    fixture
        .database
        .record_mirror_run(
            target,
            run_id,
            MirrorOperation::Fetch,
            MirrorResult::Interrupted {
                checkpoint: MirrorCheckpoint::FetchPending,
            },
        )
        .await
        .expect("interruption is durable");

    let reservation_count: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.mirror_quota_reservations where run_id = $1",
    )
    .bind(run_id)
    .fetch_one(fixture.pool())
    .await
    .expect("reservation count");
    assert_eq!(reservation_count, 0, "interruption releases capacity");

    let (outcome, failure, checkpoint): (String, Option<String>, String) = sqlx::query_as(
        "select outcome, failure_class, checkpoint from git_vault.mirror_lifecycle_runs
         where run_id = $1",
    )
    .bind(run_id)
    .fetch_one(fixture.pool())
    .await
    .expect("run evidence");
    assert_eq!(outcome, "interrupted");
    assert_eq!(failure.as_deref(), Some(FailureClass::Interrupted.code()));
    assert_eq!(checkpoint, "fetch_pending");

    fixture.cleanup().await.expect("cleanup");
}
