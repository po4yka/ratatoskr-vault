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

async fn seed_deletion_plan(
    fixture: &TestDatabase,
    target_id: Uuid,
    grace_complete: bool,
) -> (Uuid, Uuid, Uuid) {
    let (snapshot_id, artifact_id) = seed_snapshot_artifact(fixture, target_id).await;
    let policy_id = Uuid::now_v7();
    let evaluation_id = Uuid::now_v7();
    let tombstone_id = Uuid::now_v7();
    let plan_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.retention_policies
             (policy_id, name, minimum_age_seconds, grace_seconds,
              keep_last_restorable, created_at)
         values ($1, $2, 86400, 86400, 1, now())",
    )
    .bind(policy_id)
    .bind(format!("transition-{target_id}"))
    .execute(fixture.pool())
    .await
    .expect("policy fixture must insert");
    seed_evaluation_and_plan(
        fixture,
        DeletionFixtureIds {
            target: target_id,
            snapshot: snapshot_id,
            policy: policy_id,
            evaluation: evaluation_id,
            tombstone: tombstone_id,
            plan: plan_id,
        },
        grace_complete,
    )
    .await;
    (plan_id, tombstone_id, artifact_id)
}

async fn seed_snapshot_artifact(fixture: &TestDatabase, target_id: Uuid) -> (Uuid, Uuid) {
    let mirror_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let snapshot_id = Uuid::now_v7();
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(mirror_id)
    .bind(target_id)
    .bind(format!("mirrors/ef/{mirror_id}.git"))
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
    .bind(vec![5_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("snapshot fixture must insert");
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', $3, 'ratatoskr-vault', 'sha256',
                 'application/vnd.git.bundle', 1024, now())",
    )
    .bind(artifact_id)
    .bind(snapshot_id)
    .bind(vec![6_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("artifact fixture must insert");
    (snapshot_id, artifact_id)
}

struct DeletionFixtureIds {
    target: Uuid,
    snapshot: Uuid,
    policy: Uuid,
    evaluation: Uuid,
    tombstone: Uuid,
    plan: Uuid,
}

async fn seed_evaluation_and_plan(
    fixture: &TestDatabase,
    ids: DeletionFixtureIds,
    grace_complete: bool,
) {
    sqlx::query(
        "insert into git_vault.retention_evaluations
             (evaluation_id, target_id, policy_id, mode, policy_snapshot, outcome,
              correlation_id, evaluated_at)
         values ($1, $2, $3, 'scheduled', '{}'::jsonb, 'selected', $4, now())",
    )
    .bind(ids.evaluation)
    .bind(ids.target)
    .bind(ids.policy)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("evaluation fixture must insert");
    sqlx::query(
        "insert into git_vault.tombstones
             (tombstone_id, target_id, governing_policy_revision, reason, was_pinned,
              correlation_id, recorded_at, not_before)
         values ($1, $2, 2, 'policy_inactive', false, $3,
                 case when $4 then now() - interval '3 hours' else now() - interval '1 hour' end,
                 case when $4 then now() - interval '2 hours' else now() + interval '2 hours' end)",
    )
    .bind(ids.tombstone)
    .bind(ids.target)
    .bind(Uuid::now_v7())
    .bind(grace_complete)
    .execute(fixture.pool())
    .await
    .expect("tombstone fixture must insert");
    sqlx::query(
        "insert into git_vault.deletion_plans
             (plan_id, evaluation_id, target_id, snapshot_id, tombstone_id, reason,
              automatic, tombstoned_at, not_before, estimated_bytes, correlation_id)
         select $1, $2, $3, $4, tombstone_id, 'target_inactive', true,
                recorded_at, not_before, 1024, correlation_id
         from git_vault.tombstones where tombstone_id = $5",
    )
    .bind(ids.plan)
    .bind(ids.evaluation)
    .bind(ids.target)
    .bind(ids.snapshot)
    .bind(ids.tombstone)
    .execute(fixture.pool())
    .await
    .expect("plan fixture must insert");
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

/// Target deletion can start only after grace and can finish only after every linked stage and
/// plan has terminal evidence; the terminal state cannot reactivate.
#[tokio::test]
async fn deletion_transitions_require_stage_evidence() {
    let fixture = test_database().await;
    let early_target = seed_target(&fixture, "delete-early", TargetStatus::Excluded).await;
    seed_deletion_plan(&fixture, early_target, false).await;
    let early = fixture
        .database
        .set_target_status(early_target, TargetStatus::Deleting)
        .await;
    assert!(early.is_err(), "excluded -> deleting must not bypass grace");

    let target = seed_target(&fixture, "delete-evidenced", TargetStatus::Excluded).await;
    let (plan_id, tombstone_id, artifact_id) = seed_deletion_plan(&fixture, target, true).await;
    fixture
        .database
        .set_target_status(target, TargetStatus::Deleting)
        .await
        .expect("grace-complete evidenced deletion may start");
    let premature = sqlx::query(
        "update git_vault.targets set status = 'deleted', updated_at = now()
         where target_id = $1",
    )
    .bind(target)
    .execute(fixture.pool())
    .await;
    assert!(
        premature.is_err(),
        "deleting -> deleted needs completed stages"
    );

    sqlx::query(
        "insert into git_vault.deletion_stage_attempts
             (attempt_id, plan_id, stage_kind, stage_key, artifact_id, ordinal, outcome,
              lease_owner, lease_expires_at, absence_verified, started_at, finished_at)
         values ($1, $2, 'local', $3, $4, 0, 'succeeded', $5,
                 now(), true, now() - interval '1 minute', now())",
    )
    .bind(Uuid::now_v7())
    .bind(plan_id)
    .bind(format!("local:{artifact_id}"))
    .bind(artifact_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("terminal local evidence must insert");
    sqlx::query("update git_vault.deletion_plans set status = 'local_deleting' where plan_id = $1")
        .bind(plan_id)
        .execute(fixture.pool())
        .await
        .expect("plan must enter local stage");
    sqlx::query(
        "insert into git_vault.deletion_stage_attempts
             (attempt_id, plan_id, stage_kind, stage_key, ordinal, outcome,
              lease_owner, lease_expires_at, absence_verified, started_at, finished_at)
         values ($1, $2, 'mirror_local', $3, 1, 'succeeded', $4,
                 now(), true, now() - interval '1 minute', now())",
    )
    .bind(Uuid::now_v7())
    .bind(plan_id)
    .bind(format!("mirror:{target}"))
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("terminal mirror evidence must insert");
    sqlx::query(
        "update git_vault.deletion_plans set status = 'replica_deleting' where plan_id = $1",
    )
    .bind(plan_id)
    .execute(fixture.pool())
    .await
    .expect("plan must enter replica stage");
    sqlx::query(
        "update git_vault.deletion_plans
         set status = 'completed', completed_at = now() where plan_id = $1",
    )
    .bind(plan_id)
    .execute(fixture.pool())
    .await
    .expect("all evidenced plan work may complete");
    sqlx::query("update git_vault.tombstones set completed_at = now() where tombstone_id = $1")
        .bind(tombstone_id)
        .execute(fixture.pool())
        .await
        .expect("target tombstone may complete");
    sqlx::query(
        "update git_vault.targets set status = 'deleted', updated_at = now()
         where target_id = $1",
    )
    .bind(target)
    .execute(fixture.pool())
    .await
    .expect("deleting -> deleted must accept complete evidence");
    let reactivation = fixture
        .database
        .set_target_status(target, TargetStatus::Requested)
        .await;
    assert!(reactivation.is_err(), "deleted is terminal");

    fixture.cleanup().await.expect("cleanup");
}
