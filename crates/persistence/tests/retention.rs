//! Durable pin, grace, stage, and audit contracts on disposable `PostgreSQL`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::retention::{PinSource, RetentionPolicy};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{
    DeletionPlanOutcome, DeletionPlanRequest, DeletionStageKind, RetentionCandidateClassification,
    RetentionCandidateEvidence, RetentionEvaluationMode, RetentionEvaluationOutcome,
    RetentionEvaluationRequest, StageClaimOutcome, StageClaimRequest,
};
use uuid::Uuid;

struct SeededSnapshot {
    target: Uuid,
    snapshot: Uuid,
    policy: Uuid,
    evaluation: Uuid,
}

async fn seed_snapshot(fixture: &TestDatabase, suffix: &str) -> SeededSnapshot {
    let target_id = Uuid::now_v7();
    let mirror_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let snapshot_id = Uuid::now_v7();
    let policy_id = Uuid::now_v7();
    let evaluation_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'healthy', now(), now())",
    )
    .bind(target_id)
    .bind(format!("retention-{suffix}"))
    .execute(fixture.pool())
    .await
    .expect("target fixture must insert");
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(mirror_id)
    .bind(target_id)
    .bind(format!("mirrors/ab/{mirror_id}.git"))
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
    .bind(vec![7_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("snapshot fixture must insert");
    sqlx::query(
        "insert into git_vault.retention_policies
             (policy_id, name, minimum_age_seconds, grace_seconds,
              keep_last_restorable, created_at)
         values ($1, $2, 86400, 86400, 1, now())",
    )
    .bind(policy_id)
    .bind(format!("policy-{suffix}"))
    .execute(fixture.pool())
    .await
    .expect("policy fixture must insert");
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
    .expect("evaluation fixture must insert");

    SeededSnapshot {
        target: target_id,
        snapshot: snapshot_id,
        policy: policy_id,
        evaluation: evaluation_id,
    }
}

#[tokio::test]
async fn evaluation_persists_every_ordered_candidate_and_audit_event() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "evaluation-audit").await;
    let correlation_id = Uuid::now_v7();
    let evaluation_id = fixture
        .database
        .record_retention_evaluation(&RetentionEvaluationRequest {
            target_id: seeded.target,
            policy_id: seeded.policy,
            policy: RetentionPolicy {
                keep_last_restorable: 1,
                minimum_age_seconds: 86_400,
                grace_seconds: 86_400,
            },
            mode: RetentionEvaluationMode::Scheduled,
            required_bytes: None,
            outcome: RetentionEvaluationOutcome::NoCandidates,
            correlation_id,
            evaluated_at_epoch_seconds: 1_700_000_000,
            candidates: vec![RetentionCandidateEvidence {
                snapshot_id: seeded.snapshot,
                classification: RetentionCandidateClassification::ProtectedPinned,
                pin_sources: vec![PinSource::Operator, PinSource::User],
                target_inactive: false,
                estimated_bytes: 1024,
                deletion_not_before_epoch_seconds: None,
            }],
        })
        .await
        .expect("complete evaluation persistence");

    let evaluation: Option<(serde_json::Value, String)> = sqlx::query_as(
        "select policy_snapshot, outcome from git_vault.retention_evaluations
         where evaluation_id = $1",
    )
    .bind(evaluation_id)
    .fetch_optional(fixture.pool())
    .await
    .expect("evaluation query");
    assert_eq!(
        evaluation,
        Some((
            serde_json::json!({
                "grace_seconds": 86400,
                "keep_last_restorable": 1,
                "minimum_age_seconds": 86400
            }),
            "no_candidates".to_owned()
        ))
    );
    let candidate: (i32, String, serde_json::Value) = sqlx::query_as(
        "select ordinal, classification, pin_sources
         from git_vault.retention_candidates where evaluation_id = $1",
    )
    .bind(evaluation_id)
    .fetch_one(fixture.pool())
    .await
    .expect("ordered candidate query");
    assert_eq!(
        candidate,
        (
            0,
            "protected_pinned".to_owned(),
            serde_json::json!(["operator", "user"])
        )
    );
    let audit: Vec<(String, String)> = sqlx::query_as(
        "select reason, outcome from git_vault.retention_audit
         where evaluation_id = $1 order by occurred_at, audit_id",
    )
    .bind(evaluation_id)
    .fetch_all(fixture.pool())
    .await
    .expect("evaluation audit query");
    assert_eq!(
        audit,
        vec![("protected_pinned".to_owned(), "protected".to_owned())]
    );

    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn active_pin_blocks_automatic_deletion_plan() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "active-pin").await;
    sqlx::query(
        "insert into git_vault.snapshot_pins
             (pin_id, snapshot_id, source, source_reference, correlation_id, pinned_at)
         values ($1, $2, 'operator', 'fixture-operator', $3, now())",
    )
    .bind(Uuid::now_v7())
    .bind(seeded.snapshot)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("active pin fixture must insert");

    let outcome = fixture
        .database
        .create_deletion_plan(DeletionPlanRequest {
            evaluation_id: seeded.evaluation,
            target_id: seeded.target,
            snapshot_id: seeded.snapshot,
            tombstone_id: None,
            automatic: true,
            tombstoned_at_epoch_seconds: 1_700_000_000,
            not_before_epoch_seconds: 1_700_086_400,
            estimated_bytes: 1024,
            correlation_id: Uuid::now_v7(),
        })
        .await
        .expect("pin protection is a decision, not infrastructure failure");

    assert_eq!(outcome, DeletionPlanOutcome::ProtectedPinned);
    let plans: i64 =
        sqlx::query_scalar("select count(*) from git_vault.deletion_plans where snapshot_id = $1")
            .bind(seeded.snapshot)
            .fetch_one(fixture.pool())
            .await
            .expect("plan count must query");
    let refusals: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.retention_audit
         where snapshot_id = $1 and event_kind = 'refusal' and reason = 'protected_pinned'",
    )
    .bind(seeded.snapshot)
    .fetch_one(fixture.pool())
    .await
    .expect("pin refusal audit must query");
    assert_eq!(plans, 0);
    assert_eq!(refusals, 1);

    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn concurrent_pin_serializes_before_stage_claim() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "concurrent-pin").await;
    let artifact_id = seed_artifact(&fixture, seeded.snapshot).await;
    let plan_id = create_past_plan(&fixture, &seeded).await;
    let mut pin_transaction = fixture.pool().begin().await.expect("pin transaction");
    sqlx::query("select 1 from git_vault.snapshots where snapshot_id = $1 for update")
        .bind(seeded.snapshot)
        .execute(&mut *pin_transaction)
        .await
        .expect("pin must own snapshot row");
    sqlx::query(
        "insert into git_vault.snapshot_pins
             (pin_id, snapshot_id, source, source_reference, correlation_id, pinned_at)
         values ($1, $2, 'user', 'concurrent-user', $3, now())",
    )
    .bind(Uuid::now_v7())
    .bind(seeded.snapshot)
    .bind(Uuid::now_v7())
    .execute(&mut *pin_transaction)
    .await
    .expect("uncommitted pin evidence");
    let database = fixture.database.clone();
    let claim = tokio::spawn(async move {
        database
            .claim_deletion_stage(&StageClaimRequest {
                plan_id,
                kind: DeletionStageKind::Local,
                stage_key: format!("local:{artifact_id}"),
                artifact_id: Some(artifact_id),
                replica_target_id: None,
                placement_id: None,
                lease_owner: Uuid::now_v7(),
                lease_seconds: 60,
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !claim.is_finished(),
        "claim must wait for the authoritative snapshot pin lock"
    );
    pin_transaction.commit().await.expect("pin commit");

    assert_eq!(
        claim.await.expect("claim task").expect("claim decision"),
        StageClaimOutcome::ProtectedPinned
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn concurrent_shared_digest_serializes_before_delete_claim() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "concurrent-shared").await;
    let artifact_id = seed_artifact(&fixture, seeded.snapshot).await;
    let plan_id = create_past_plan(&fixture, &seeded).await;
    let (mirror_id, run_id): (Uuid, Uuid) = sqlx::query_as(
        "select mirror_id, mirror_lifecycle_run_id from git_vault.snapshots
         where snapshot_id = $1",
    )
    .bind(seeded.snapshot)
    .fetch_one(fixture.pool())
    .await
    .expect("snapshot storage identity");
    let digest_key = format!("local_digest:{}", "0b".repeat(32));
    let mut publication = fixture
        .pool()
        .begin()
        .await
        .expect("publication transaction");
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&digest_key)
        .execute(&mut *publication)
        .await
        .expect("physical identity lock");
    let database = fixture.database.clone();
    let claim = tokio::spawn(async move {
        database
            .claim_deletion_stage(&StageClaimRequest {
                plan_id,
                kind: DeletionStageKind::Local,
                stage_key: format!("local:{artifact_id}"),
                artifact_id: Some(artifact_id),
                replica_target_id: None,
                placement_id: None,
                lease_owner: Uuid::now_v7(),
                lease_seconds: 60,
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let retained_snapshot = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.snapshots
             (snapshot_id, target_id, mirror_id, mirror_lifecycle_run_id, format, status,
              refs_hash, created_at)
         values ($1, $2, $3, $4, 'git_bundle', 'restorable', $5, now())",
    )
    .bind(retained_snapshot)
    .bind(seeded.target)
    .bind(mirror_id)
    .bind(run_id)
    .bind(vec![12_u8; 32])
    .execute(&mut *publication)
    .await
    .expect("retained snapshot");
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', $3, 'ratatoskr-vault', 'sha256',
                 'application/vnd.git.bundle', 1024, now())",
    )
    .bind(Uuid::now_v7())
    .bind(retained_snapshot)
    .bind(vec![11_u8; 32])
    .execute(&mut *publication)
    .await
    .expect("retained shared artifact");
    publication.commit().await.expect("publication commit");

    assert!(matches!(
        claim.await.expect("claim task").expect("claim decision"),
        StageClaimOutcome::SharedReferenceRetained { .. }
    ));
    fixture.cleanup().await.expect("cleanup");
}

async fn seed_artifact(fixture: &TestDatabase, snapshot_id: Uuid) -> Uuid {
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', $3, 'ratatoskr-vault', 'sha256',
                 'application/vnd.git.bundle', 1024, now())",
    )
    .bind(artifact_id)
    .bind(snapshot_id)
    .bind(vec![11_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("artifact fixture must insert");
    artifact_id
}

async fn create_past_plan(fixture: &TestDatabase, seeded: &SeededSnapshot) -> Uuid {
    let outcome = fixture
        .database
        .create_deletion_plan(DeletionPlanRequest {
            evaluation_id: seeded.evaluation,
            target_id: seeded.target,
            snapshot_id: seeded.snapshot,
            tombstone_id: None,
            automatic: true,
            tombstoned_at_epoch_seconds: 1_700_000_000,
            not_before_epoch_seconds: 1_700_086_400,
            estimated_bytes: 1024,
            correlation_id: Uuid::now_v7(),
        })
        .await
        .expect("past plan must persist");
    match outcome {
        DeletionPlanOutcome::Created(plan_id) => Some(plan_id),
        DeletionPlanOutcome::ProtectedPinned => None,
    }
    .expect("fixture creates its pin only after the plan")
}

#[tokio::test]
async fn deletion_claim_before_not_before_is_refused_and_journaled() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "grace-active").await;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', $3, 'ratatoskr-vault', 'sha256',
                 'application/vnd.git.bundle', 1024, now())",
    )
    .bind(artifact_id)
    .bind(seeded.snapshot)
    .bind(vec![9_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("artifact fixture must insert");
    let plan_id = match fixture
        .database
        .create_deletion_plan(DeletionPlanRequest {
            evaluation_id: seeded.evaluation,
            target_id: seeded.target,
            snapshot_id: seeded.snapshot,
            tombstone_id: None,
            automatic: true,
            tombstoned_at_epoch_seconds: 3_999_900_000,
            not_before_epoch_seconds: 4_000_000_000,
            estimated_bytes: 1024,
            correlation_id: Uuid::now_v7(),
        })
        .await
        .expect("future plan must persist")
    {
        DeletionPlanOutcome::Created(plan_id) => plan_id,
        DeletionPlanOutcome::ProtectedPinned => {
            fixture.cleanup().await.expect("cleanup");
            assert_eq!(
                DeletionPlanOutcome::ProtectedPinned,
                DeletionPlanOutcome::Created(Uuid::nil()),
                "fixture has no pin"
            );
            return;
        }
    };

    let outcome = fixture
        .database
        .claim_deletion_stage(&StageClaimRequest {
            plan_id,
            kind: DeletionStageKind::Local,
            stage_key: format!("local:{artifact_id}"),
            artifact_id: Some(artifact_id),
            replica_target_id: None,
            placement_id: None,
            lease_owner: Uuid::now_v7(),
            lease_seconds: 60,
        })
        .await
        .expect("active grace is a refusal outcome");

    assert_eq!(outcome, StageClaimOutcome::GraceActive);
    let stages: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.deletion_stage_attempts where plan_id = $1",
    )
    .bind(plan_id)
    .fetch_one(fixture.pool())
    .await
    .expect("stage count must query");
    let refusals: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.retention_audit
         where plan_id = $1 and event_kind = 'refusal' and reason = 'grace_active'",
    )
    .bind(plan_id)
    .fetch_one(fixture.pool())
    .await
    .expect("grace refusal audit must query");
    assert_eq!(stages, 0);
    assert_eq!(refusals, 1);

    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn snapshot_audit_survives_completed_deletion_in_stable_order() {
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let seeded = seed_snapshot(&fixture, "audit-after-delete").await;
    let correlation_id = Uuid::now_v7();
    for (audit, kind, reason, outcome) in [
        (1_u128, "evaluation", "eligible_ordinary", "selected"),
        (2_u128, "plan", "ordinary_retention", "planned"),
        (3_u128, "stage", "replica", "succeeded"),
    ] {
        sqlx::query(
            "insert into git_vault.retention_audit
                 (audit_id, target_id, snapshot_id, event_kind, reason, outcome,
                  correlation_id, details, occurred_at)
             values ($1, $2, $3, $4, $5, $6, $7, '{}'::jsonb,
                     to_timestamp(1700000000))",
        )
        .bind(Uuid::from_u128(audit))
        .bind(seeded.target)
        .bind(seeded.snapshot)
        .bind(kind)
        .bind(reason)
        .bind(outcome)
        .bind(correlation_id)
        .execute(fixture.pool())
        .await
        .expect("audit fixture must insert");
    }
    sqlx::query("update git_vault.snapshots set status = 'deleted' where snapshot_id = $1")
        .bind(seeded.snapshot)
        .execute(fixture.pool())
        .await
        .expect("fixture snapshot must become logically deleted");

    let audit = fixture
        .database
        .snapshot_retention_audit(seeded.snapshot, None, 10)
        .await
        .expect("deleted snapshot audit must remain queryable");

    assert_eq!(
        audit
            .iter()
            .map(|record| record.audit_id)
            .collect::<Vec<_>>(),
        [Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)]
    );
    assert_eq!(audit[0].event_kind, "evaluation");
    assert_eq!(audit[2].outcome, "succeeded");
    let target_audit = fixture
        .database
        .target_retention_audit(seeded.target, None, 10)
        .await
        .expect("target audit must use the same stable evidence");
    assert_eq!(target_audit, audit);

    fixture.cleanup().await.expect("cleanup");
}
