//! The `ingest_delivery` contract: at-least-once transport meets the inbox, and replays leave no
//! second evidence row behind.
//!
//! The first delivery of a `(source, message_id)` pair ingests normally; a redelivery is refused
//! as [`VaultError::DuplicateDelivery`] and the revision table keeps exactly the rows the first
//! delivery wrote.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

const TOMBSTONE_GRACE_SECONDS: i64 = 2_592_000;

async fn test_database() -> TestDatabase {
    let _subscriber = tracing_subscriber::fmt().with_test_writer().try_init();
    TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied")
}

/// The governing record a desired-state message carries.
fn governing(policy_revision: u64) -> ValidatedDelivery {
    ValidatedDelivery {
        preservation_level: "git_mirror".to_owned(),
        pinned: Some(false),
        include_wiki: Some(false),
        include_releases: Some(true),
        include_issues: Some(true),
        offsite_required: Some(true),
        correlation_id: "018f3c1a-7b2c-7d4e-8f9a-000000000004".to_owned(),
        policy_revision: Some(policy_revision),
    }
}

/// How many revision-evidence rows exist for the pair.
async fn revision_rows(fixture: &TestDatabase, target_id: Uuid, policy_revision: i64) -> i64 {
    sqlx::query_scalar(
        "select count(*) from git_vault.desired_state_revisions
         where target_id = $1 and policy_revision = $2",
    )
    .bind(target_id)
    .bind(policy_revision)
    .fetch_one(fixture.pool())
    .await
    .expect("the revision count must run")
}

async fn seed_restorable_snapshot(fixture: &TestDatabase, target_id: Uuid) -> Uuid {
    let mirror_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let snapshot_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(mirror_id)
    .bind(target_id)
    .bind(format!("mirrors/cd/{mirror_id}.git"))
    .execute(fixture.pool())
    .await
    .expect("mirror fixture must insert");
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, created_at)
         values ($1, $2, 'fetch', 'succeeded', now())",
    )
    .bind(run_id)
    .bind(target_id)
    .execute(fixture.pool())
    .await
    .expect("run fixture must insert");
    sqlx::query(
        "insert into git_vault.snapshots
             (snapshot_id, target_id, mirror_id, mirror_lifecycle_run_id, format, status,
              refs_hash, created_at)
         values ($1, $2, $3, $4, 'git_bundle', 'restorable', $5, now())",
    )
    .bind(snapshot_id)
    .bind(target_id)
    .bind(mirror_id)
    .bind(run_id)
    .bind(vec![4_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("snapshot fixture must insert");
    snapshot_id
}

/// A redelivered `(source, message_id)` pair is refused without side effects: the revision table
/// still holds exactly the one row the first delivery recorded.
#[tokio::test]
async fn duplicate_delivery_is_rejected_without_side_effects() {
    let fixture = test_database().await;

    let message_id = Uuid::now_v7();
    let delivery = governing(5);
    let target_id = fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-duplicate",
            "github-catalog",
            message_id,
            &delivery,
        )
        .await
        .expect("the first delivery must be ingested");

    let replay = fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-duplicate",
            "github-catalog",
            message_id,
            &delivery,
        )
        .await;

    assert_eq!(replay, Err(VaultError::DuplicateDelivery));
    assert_eq!(revision_rows(&fixture, target_id, 5).await, 1);

    fixture.cleanup().await.expect("cleanup");
}

/// An out-of-order delivery keeps both revisions as evidence while only the maximum number
/// governs: revision 3 arriving after revision 5 must not take over planning.
#[tokio::test]
async fn stale_revisions_are_kept_but_never_govern() {
    let fixture = test_database().await;

    let newest = Uuid::now_v7();
    let target_id = fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-stale",
            "github-catalog",
            newest,
            &governing(5),
        )
        .await
        .expect("the newest delivery must be ingested");

    let stale = Uuid::now_v7();
    fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-stale",
            "github-catalog",
            stale,
            &governing(3),
        )
        .await
        .expect("the stale delivery must be kept as evidence");

    assert_eq!(revision_rows(&fixture, target_id, 5).await, 1);
    assert_eq!(revision_rows(&fixture, target_id, 3).await, 1);

    let governed = fixture
        .database
        .governing_policy(target_id)
        .await
        .expect("the governing read must run")
        .expect("a target with revisions must have a governor");
    assert_eq!(governed.policy_revision, 5);

    fixture.cleanup().await.expect("cleanup");
}

/// Two ingests racing on one target through separate connections both commit their revision,
/// and the maximum governs afterwards. The target-row lock (design D4) serializes the writers;
/// this test keeps that guarantee pinned under repeated parallel runs.
#[tokio::test]
async fn conflicting_ingests_serialize_on_target_row() {
    let fixture = test_database().await;

    let database_for_high = fixture.database.clone();
    let high = tokio::spawn(async move {
        database_for_high
            .ingest_delivery(
                "github",
                "ingest-race",
                "github-catalog",
                Uuid::now_v7(),
                &governing(9),
            )
            .await
    });

    let database_for_low = fixture.database.clone();
    let low = tokio::spawn(async move {
        database_for_low
            .ingest_delivery(
                "github",
                "ingest-race",
                "github-catalog",
                Uuid::now_v7(),
                &governing(7),
            )
            .await
    });

    let high = high.await.expect("the higher task must join");
    let low = low.await.expect("the lower task must join");
    high.expect("the higher ingest must commit");
    low.expect("the lower ingest must commit");

    let target_id: Uuid = sqlx::query_scalar(
        "select target_id from git_vault.targets
         where provider = $1 and external_repository_id = $2",
    )
    .bind("github")
    .bind("ingest-race")
    .fetch_one(fixture.pool())
    .await
    .expect("the raced target must exist");

    assert_eq!(revision_rows(&fixture, target_id, 9).await, 1);
    assert_eq!(revision_rows(&fixture, target_id, 7).await, 1);

    let governed = fixture
        .database
        .governing_policy(target_id)
        .await
        .expect("the governing read must run")
        .expect("a target with revisions must have a governor");
    assert_eq!(governed.policy_revision, 9);

    fixture.cleanup().await.expect("cleanup");
}

/// A governing inactive policy starts one fixed grace window, and a newer active policy cancels
/// its still-pending deletion intent without removing snapshot evidence.
#[tokio::test]
async fn none_policy_tombstones_once_and_reactivation_cancels_before_deletion() {
    use ratatoskr_vault_core::target_state::TargetStatus;

    let fixture = test_database().await;
    let target_id = fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-tombstone",
            "github-catalog",
            Uuid::now_v7(),
            &governing(1),
        )
        .await
        .expect("active policy must enroll");
    let snapshot_id = seed_restorable_snapshot(&fixture, target_id).await;
    let mut inactive = governing(2);
    inactive.preservation_level = "none".to_owned();
    fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-tombstone",
            "github-catalog",
            Uuid::now_v7(),
            &inactive,
        )
        .await
        .expect("inactive revision must persist");
    fixture
        .database
        .apply_transition(target_id, TargetStatus::Excluded, &inactive)
        .await
        .expect("inactive target must converge");
    fixture
        .database
        .apply_transition(target_id, TargetStatus::Excluded, &inactive)
        .await
        .expect("reconvergence must reuse the tombstone");

    let tombstone: Option<(Uuid, i64)> = sqlx::query_as(
        "select tombstone_id,
                extract(epoch from not_before - recorded_at)::bigint
         from git_vault.tombstones
         where target_id = $1 and cancelled_at is null and completed_at is null",
    )
    .bind(target_id)
    .fetch_optional(fixture.pool())
    .await
    .expect("active tombstone must query");
    assert_eq!(
        tombstone.map(|(_, grace_seconds)| grace_seconds),
        Some(TOMBSTONE_GRACE_SECONDS)
    );
    let Some((tombstone_id, _grace_seconds)) = tombstone else {
        fixture.cleanup().await.expect("cleanup");
        return;
    };
    assert_eq!(active_tombstone_count(&fixture, target_id).await, 1);

    seed_pending_plan(&fixture, target_id, snapshot_id, tombstone_id).await;

    let reactivated = governing(3);
    fixture
        .database
        .ingest_delivery(
            "github",
            "ingest-tombstone",
            "github-catalog",
            Uuid::now_v7(),
            &reactivated,
        )
        .await
        .expect("reactivated revision must persist");
    fixture
        .database
        .apply_transition(target_id, TargetStatus::Requested, &reactivated)
        .await
        .expect("reactivation before effects must converge");

    let (status, cancelled_tombstones, cancelled_plans, snapshots): (String, i64, i64, i64) =
        sqlx::query_as(
            "select targets.status,
                    (select count(*) from git_vault.tombstones
                     where target_id = $1 and cancelled_at is not null),
                    (select count(*) from git_vault.deletion_plans
                     where target_id = $1 and status = 'cancelled'),
                    (select count(*) from git_vault.snapshots where snapshot_id = $2)
             from git_vault.targets where target_id = $1",
        )
        .bind(target_id)
        .bind(snapshot_id)
        .fetch_one(fixture.pool())
        .await
        .expect("reactivated state must query");
    assert_eq!(status, "requested");
    assert_eq!(cancelled_tombstones, 1);
    assert_eq!(cancelled_plans, 1);
    assert_eq!(snapshots, 1);

    fixture.cleanup().await.expect("cleanup");
}

async fn seed_pending_plan(
    fixture: &TestDatabase,
    target_id: Uuid,
    snapshot_id: Uuid,
    tombstone_id: Uuid,
) {
    let policy_id = Uuid::now_v7();
    let evaluation_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.retention_policies
             (policy_id, name, minimum_age_seconds, grace_seconds,
              keep_last_restorable, created_at)
         values ($1, $2, 86400, $3, 1, now())",
    )
    .bind(policy_id)
    .bind(format!("withdrawal-{target_id}"))
    .bind(TOMBSTONE_GRACE_SECONDS)
    .execute(fixture.pool())
    .await
    .expect("retention policy must insert");
    sqlx::query(
        "insert into git_vault.retention_evaluations
             (evaluation_id, target_id, policy_id, mode, policy_snapshot, outcome,
              correlation_id, evaluated_at)
         values ($1, $2, $3, 'scheduled', '{}'::jsonb, 'selected', $4, now())",
    )
    .bind(evaluation_id)
    .bind(target_id)
    .bind(policy_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("evaluation must insert");
    sqlx::query(
        "insert into git_vault.deletion_plans
             (plan_id, evaluation_id, target_id, snapshot_id, tombstone_id, reason,
              automatic, tombstoned_at, not_before, estimated_bytes, correlation_id)
         select $1, $2, $3, $4, tombstone_id, 'target_inactive', true,
                recorded_at, not_before, 1024, correlation_id
         from git_vault.tombstones where tombstone_id = $5",
    )
    .bind(Uuid::now_v7())
    .bind(evaluation_id)
    .bind(target_id)
    .bind(snapshot_id)
    .bind(tombstone_id)
    .execute(fixture.pool())
    .await
    .expect("pending plan must insert");
}

async fn active_tombstone_count(fixture: &TestDatabase, target_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from git_vault.tombstones
         where target_id = $1 and cancelled_at is null and completed_at is null",
    )
    .bind(target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("tombstone count must query")
}
