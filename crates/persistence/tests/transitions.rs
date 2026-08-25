//! The `apply_transition` contract: one transaction owns the status move, its revision evidence,
//! and the state-changed event.
//!
//! A legal move writes the new status and exactly one outbox row carrying `target_id`,
//! `from_status`, `to_status`, `policy_revision` and `correlation_id`. A same-status call is an
//! annotation that emits no events. Anything failing mid-flight leaves no partial evidence.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::target_state::TargetStatus;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

const CORR_LEGAL: &str = "018f3c1a-7b2c-7d4e-8f9a-000000000001";
const CORR_ANNOTATION: &str = "018f3c1a-7b2c-7d4e-8f9a-000000000002";
const CORR_REFUSED: &str = "018f3c1a-7b2c-7d4e-8f9a-000000000003";

async fn test_database() -> TestDatabase {
    TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied")
}

/// The governing revision a transition is applied under.
fn governing(policy_revision: u64, correlation_id: &str) -> ValidatedDelivery {
    ValidatedDelivery {
        preservation_level: "git_mirror".to_owned(),
        pinned: Some(false),
        include_wiki: Some(false),
        include_releases: Some(true),
        include_issues: Some(true),
        offsite_required: Some(true),
        correlation_id: correlation_id.to_owned(),
        policy_revision: Some(policy_revision),
    }
}

/// Seeds one target at `status` and hands back its id.
async fn seed_target(fixture: &TestDatabase, suffix: &str, status: TargetStatus) -> Uuid {
    let target_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, $3, now(), now())",
    )
    .bind(target_id)
    .bind(format!("transitions-{suffix}"))
    .bind(status.as_str())
    .execute(fixture.pool())
    .await
    .expect("the fixture insert must run");
    target_id
}

/// The status currently stored for the target.
async fn stored_status(fixture: &TestDatabase, target_id: Uuid) -> String {
    sqlx::query_scalar("select status from git_vault.targets where target_id = $1")
        .bind(target_id)
        .fetch_one(fixture.pool())
        .await
        .expect("the status read must run")
}

/// How many state-changed events the target owes the bus.
async fn outbox_rows(fixture: &TestDatabase, target_id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from git_vault.outbox where aggregate_id = $1")
        .bind(target_id)
        .fetch_one(fixture.pool())
        .await
        .expect("the outbox count must run")
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

/// A legal move persists the new status and exactly one event whose payload names everything
/// the machine needs to audit the change.
#[tokio::test]
async fn legal_move_persists_status_and_exactly_one_state_changed_event() {
    let fixture = test_database().await;
    let target = seed_target(&fixture, "legal", TargetStatus::Requested).await;

    fixture
        .database
        .apply_transition(target, TargetStatus::Cloning, &governing(7, CORR_LEGAL))
        .await
        .expect("a legal move must be applied");

    assert_eq!(stored_status(&fixture, target).await, "cloning");
    assert_eq!(outbox_rows(&fixture, target).await, 1);

    let (event_type, payload_target, from_status, to_status, policy_revision, correlation_id): (
        String,
        String,
        String,
        String,
        i64,
        String,
    ) = sqlx::query_as(
        "select event_type,
                payload->>'target_id',
                payload->>'from_status',
                payload->>'to_status',
                (payload->>'policy_revision')::bigint,
                payload->>'correlation_id'
         from git_vault.outbox
         where aggregate_id = $1 and event_type = 'vault.target.state_changed.v1'",
    )
    .bind(target)
    .fetch_one(fixture.pool())
    .await
    .expect("the event row must exist");

    assert_eq!(event_type, "vault.target.state_changed.v1");
    assert_eq!(payload_target, target.to_string());
    assert_eq!(from_status, "requested");
    assert_eq!(to_status, "cloning");
    assert_eq!(policy_revision, 7);
    assert_eq!(correlation_id, CORR_LEGAL);

    fixture.cleanup().await.expect("cleanup");
}

/// Writing the status the target already carries is an annotation: persisted, silent.
#[tokio::test]
async fn same_status_call_annotates_without_events() {
    let fixture = test_database().await;
    let target = seed_target(&fixture, "annotation", TargetStatus::Ready).await;

    fixture
        .database
        .apply_transition(target, TargetStatus::Ready, &governing(8, CORR_ANNOTATION))
        .await
        .expect("a same-status annotation must be accepted");

    assert_eq!(stored_status(&fixture, target).await, "ready");
    assert_eq!(outbox_rows(&fixture, target).await, 0);

    fixture.cleanup().await.expect("cleanup");
}

/// A guard refusal after the revision insert rolls the whole transaction back: neither the
/// status, nor the revision evidence, nor any event survives a refused move.
#[tokio::test]
async fn guard_refusal_mid_transaction_leaves_no_evidence() {
    let fixture = test_database().await;
    let target = seed_target(&fixture, "refused", TargetStatus::Excluded).await;

    let outcome = fixture
        .database
        .apply_transition(target, TargetStatus::Ready, &governing(9, CORR_REFUSED))
        .await;

    assert!(outcome.is_err(), "excluded -> ready must be refused");
    assert_eq!(stored_status(&fixture, target).await, "excluded");
    assert_eq!(revision_rows(&fixture, target, 9).await, 0);
    assert_eq!(outbox_rows(&fixture, target).await, 0);

    fixture.cleanup().await.expect("cleanup");
}
