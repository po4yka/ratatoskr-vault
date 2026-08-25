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

async fn test_database() -> TestDatabase {
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
