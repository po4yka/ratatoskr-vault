//! Off-host replica inventory and attempt persistence tests.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use ratatoskr_vault_core::snapshot::{BlobRef, LfsEvidence, LfsObjectEvidence};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{
    DeletionStageKind, ReplicaTargetObservation, SnapshotSource, StageClaimOutcome,
    StageClaimRequest,
};
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn required_replication_inventory_includes_every_manifest_lfs_object() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let lfs = LfsEvidence::new(
        "git-lfs/fixture".to_owned(),
        vec![
            LfsObjectEvidence {
                oid: "3".repeat(64),
                blob: blob("3", "application/octet-stream"),
            },
            LfsObjectEvidence {
                oid: "4".repeat(64),
                blob: blob("4", "application/octet-stream"),
            },
        ],
    );
    let snapshot_id = fixture
        .database
        .record_built_snapshot(
            healthy_source(&fixture).await,
            None,
            &blob("1", "application/vnd.git.bundle"),
            &blob("2", "application/json"),
            &"5".repeat(64),
            Some(&lfs),
        )
        .await
        .expect("LFS snapshot");
    let replica_target_id = Uuid::now_v7();
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id,
            name: "lfs-offsite".to_owned(),
            endpoint_origin: "https://s3.example.invalid".to_owned(),
            bucket: "vault".to_owned(),
            key_prefix: "archive".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("replica target");

    let due = fixture
        .database
        .due_replication_units(replica_target_id, 0, 8)
        .await
        .expect("complete inventory");
    assert_eq!(
        due.len(),
        4,
        "bundle, manifest, and both LFS objects are due"
    );
    assert!(due.iter().all(|unit| unit.snapshot_id == snapshot_id));
    let digests: std::collections::BTreeSet<_> =
        due.iter().map(|unit| unit.blob.sha256.clone()).collect();
    assert_eq!(
        digests,
        std::collections::BTreeSet::from([
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64),
            "4".repeat(64),
        ])
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn replica_inventory_records_location_and_rejects_terminal_rewrite() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let artifact_id = stored_bundle(&fixture).await;
    let replica_target_id = Uuid::now_v7();
    let attempt_id = Uuid::now_v7();
    let placement_id = Uuid::now_v7();

    sqlx::query(
        "insert into git_vault.replica_targets
             (replica_target_id, name, endpoint_origin, bucket, key_prefix, required, enabled,
              first_seen_at, last_seen_at)
         values ($1, 'offsite', 'https://s3.example.invalid', 'vault', 'archive', true, true,
                 now(), now())",
    )
    .bind(replica_target_id)
    .execute(fixture.pool())
    .await
    .expect("replica target inventory");
    sqlx::query(
        "insert into git_vault.replication_attempts
             (attempt_id, artifact_id, replica_target_id, outcome, lease_owner, lease_expires_at,
              remote_hash, remote_size_bytes, started_at, finished_at)
         values ($1, $2, $3, 'succeeded', $4, now() + interval '1 minute',
                 decode(repeat('1', 64), 'hex'), 1, now(), now())",
    )
    .bind(attempt_id)
    .bind(artifact_id)
    .bind(replica_target_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("terminal replication attempt");
    sqlx::query(
        "insert into git_vault.replica_placements
             (placement_id, artifact_id, replica_target_id, object_key, sha256_hash, size_bytes,
              first_placed_at, last_verified_at, last_attempt_id)
         values ($1, $2, $3, $4, decode(repeat('1', 64), 'hex'), 1, now(), now(), $5)",
    )
    .bind(placement_id)
    .bind(artifact_id)
    .bind(replica_target_id)
    .bind("archive/sha256/11/1111111111111111111111111111111111111111111111111111111111111111")
    .bind(attempt_id)
    .execute(fixture.pool())
    .await
    .expect("verified placement inventory");

    let inventory: (String, String, String, bool, Vec<u8>, i64, bool) = sqlx::query_as(
        "select replica_targets.name, replica_targets.endpoint_origin, replica_placements.object_key,
                replica_targets.required, replica_placements.sha256_hash,
                replica_placements.size_bytes, replica_placements.last_verified_at is not null
         from git_vault.replica_placements
         join git_vault.replica_targets using (replica_target_id)
         where placement_id = $1",
    )
    .bind(placement_id)
    .fetch_one(fixture.pool())
    .await
    .expect("placement inventory");
    assert_eq!(inventory.0, "offsite");
    assert_eq!(inventory.1, "https://s3.example.invalid");
    assert!(inventory.2.starts_with("archive/sha256/11/"));
    assert!(inventory.3);
    assert_eq!(inventory.4, vec![0x11; 32]);
    assert_eq!(inventory.5, 1);
    assert!(inventory.6);

    let rewrite = sqlx::query(
        "update git_vault.replication_attempts set failure_class = 'rewritten' where attempt_id = $1",
    )
    .bind(attempt_id)
    .execute(fixture.pool())
    .await;
    assert!(
        rewrite.is_err(),
        "terminal attempt evidence must be append-only"
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn reverification_appends_attempt_and_advances_inventory() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let artifact_id = stored_bundle(&fixture).await;
    let replica_target_id = Uuid::now_v7();
    let observed_id = fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id,
            name: "offsite".to_owned(),
            endpoint_origin: "https://s3.example.invalid".to_owned(),
            bucket: "vault".to_owned(),
            key_prefix: "archive".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("target observation");
    assert_eq!(observed_id, replica_target_id);
    let owner = Uuid::now_v7();
    let first_attempt = fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            replica_target_id,
            owner,
            Duration::from_mins(1),
        )
        .await
        .expect("first claim");
    let expected = blob("1", "application/vnd.git.bundle");
    let object_key =
        "archive/sha256/11/1111111111111111111111111111111111111111111111111111111111111111";
    let first = fixture
        .database
        .record_verified_replica(first_attempt, owner, object_key, &expected)
        .await
        .expect("first verified placement");
    sqlx::query("select pg_sleep(0.01)")
        .execute(fixture.pool())
        .await
        .expect("separate evidence timestamps");
    let second_attempt = fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            replica_target_id,
            owner,
            Duration::from_mins(1),
        )
        .await
        .expect("re-verification claim");
    let second = fixture
        .database
        .record_verified_replica(second_attempt, owner, object_key, &expected)
        .await
        .expect("re-verified placement");

    let evidence: (i64, Uuid, Uuid, bool, bool) = sqlx::query_as(
        "select count(attempts.attempt_id), placements.placement_id,
                placements.last_attempt_id,
                placements.last_verified_at > placements.first_placed_at,
                placements.last_verified_at = max(attempts.finished_at)
         from git_vault.replication_attempts attempts
         join git_vault.replica_placements placements
           on placements.artifact_id = attempts.artifact_id
          and placements.replica_target_id = attempts.replica_target_id
         where attempts.artifact_id = $1 and attempts.replica_target_id = $2
         group by placements.placement_id",
    )
    .bind(artifact_id)
    .bind(replica_target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("append-only re-verification evidence");
    assert_eq!(evidence.0, 2);
    assert_eq!(evidence.1, first.placement_id);
    assert_eq!(evidence.1, second.placement_id);
    assert_eq!(evidence.2, second_attempt);
    assert!(evidence.3, "inventory verification time must advance");
    assert!(
        evidence.4,
        "inventory time must be the terminal attempt time, not an unrelated later clock read"
    );
    let due = fixture
        .database
        .due_replication_units(
            replica_target_id,
            second.last_verified_at.saturating_add(1),
            3,
        )
        .await
        .expect("bounded stale-placement read");
    assert_eq!(
        due.len(),
        2,
        "the stale bundle and still-absent manifest must both be due before the cutoff"
    );
    assert!(due.iter().any(|unit| unit.artifact_id == artifact_id));
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn expired_claim_is_recoverable_without_duplicate_live_attempt() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let artifact_id = stored_bundle(&fixture).await;
    let replica_target_id = Uuid::now_v7();
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id,
            name: "recoverable".to_owned(),
            endpoint_origin: "https://s3.example.invalid".to_owned(),
            bucket: "vault".to_owned(),
            key_prefix: "archive".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("target observation");
    let expired_attempt = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.replication_attempts
             (attempt_id, artifact_id, replica_target_id, outcome, lease_owner,
              lease_expires_at, started_at)
         values ($1, $2, $3, 'running', $4, now() - interval '1 second',
                 now() - interval '1 minute')",
    )
    .bind(expired_attempt)
    .bind(artifact_id)
    .bind(replica_target_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("expired claim fixture");

    let replacement = fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            replica_target_id,
            Uuid::now_v7(),
            Duration::from_mins(1),
        )
        .await
        .expect("expired attempt must be abandoned before a replacement is claimed");

    assert_ne!(replacement, expired_attempt);
    let outcomes: Vec<(Uuid, String)> = sqlx::query_as(
        "select attempt_id, outcome from git_vault.replication_attempts
         where artifact_id = $1 and replica_target_id = $2 order by started_at",
    )
    .bind(artifact_id)
    .bind(replica_target_id)
    .fetch_all(fixture.pool())
    .await
    .expect("attempt history");
    assert_eq!(
        outcomes,
        vec![
            (expired_attempt, "abandoned".to_owned()),
            (replacement, "running".to_owned()),
        ]
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn deletion_staged_artifacts_are_not_due_or_claimable() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let artifact_id = stored_bundle(&fixture).await;
    let (snapshot_id, target_id): (Uuid, Uuid) = sqlx::query_as(
        "select artifacts.snapshot_id, snapshots.target_id
         from git_vault.snapshot_artifacts artifacts
         join git_vault.snapshots using (snapshot_id)
         where artifacts.artifact_id = $1",
    )
    .bind(artifact_id)
    .fetch_one(fixture.pool())
    .await
    .expect("artifact owner");
    let replica_target_id = Uuid::now_v7();
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id,
            name: "delete-staged".to_owned(),
            endpoint_origin: "https://s3.example.invalid".to_owned(),
            bucket: "vault".to_owned(),
            key_prefix: "archive".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("target observation");
    let plan_id =
        seed_replication_deletion_plan(&fixture, artifact_id, snapshot_id, target_id).await;
    let stage = fixture
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
        .expect("deletion stage claim");
    assert!(matches!(stage, StageClaimOutcome::Claimed { .. }));

    let due = fixture
        .database
        .due_replication_units(replica_target_id, 0, 8)
        .await
        .expect("due replication read");
    assert!(
        due.iter().all(|unit| unit.artifact_id != artifact_id),
        "deletion-staged artifact must not be due"
    );
    let replication_claim = fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            replica_target_id,
            Uuid::now_v7(),
            Duration::from_mins(1),
        )
        .await;
    assert!(
        replication_claim.is_err(),
        "deletion-staged artifact must not admit a replication claim"
    );

    fixture.cleanup().await.expect("cleanup");
}

async fn seed_replication_deletion_plan(
    fixture: &TestDatabase,
    artifact_id: Uuid,
    snapshot_id: Uuid,
    target_id: Uuid,
) -> Uuid {
    let policy_id = Uuid::now_v7();
    let evaluation_id = Uuid::now_v7();
    let plan_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.retention_policies
             (policy_id, name, minimum_age_seconds, grace_seconds,
              keep_last_restorable, created_at)
         values ($1, $2, 0, 60, 1, now())",
    )
    .bind(policy_id)
    .bind(format!("delete-staged-{artifact_id}"))
    .execute(fixture.pool())
    .await
    .expect("policy");
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
    .expect("evaluation");
    sqlx::query(
        "insert into git_vault.deletion_plans
             (plan_id, evaluation_id, target_id, snapshot_id, reason, automatic,
              tombstoned_at, not_before, estimated_bytes, correlation_id)
         values ($1, $2, $3, $4, 'ordinary_retention', true,
                 now() - interval '2 hours', now() - interval '1 hour', 1, $5)",
    )
    .bind(plan_id)
    .bind(evaluation_id)
    .bind(target_id)
    .bind(snapshot_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("plan");
    plan_id
}

async fn stored_bundle(fixture: &TestDatabase) -> Uuid {
    let source = healthy_source(fixture).await;
    let bundle = blob("1", "application/vnd.git.bundle");
    let snapshot_id = fixture
        .database
        .record_built_snapshot(
            source,
            None,
            &bundle,
            &blob("2", "application/json"),
            &"3".repeat(64),
            None,
        )
        .await
        .expect("snapshot");
    sqlx::query_scalar(
        "select artifact_id from git_vault.snapshot_artifacts
         where snapshot_id = $1 and kind = 'git_bundle'",
    )
    .bind(snapshot_id)
    .fetch_one(fixture.pool())
    .await
    .expect("bundle artifact")
}

async fn healthy_source(fixture: &TestDatabase) -> SnapshotSource {
    let source = SnapshotSource {
        target_id: Uuid::now_v7(),
        mirror_id: Uuid::now_v7(),
        mirror_lifecycle_run_id: Uuid::now_v7(),
    };
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'ready', now(), now())",
    )
    .bind(source.target_id)
    .bind(Uuid::now_v7().simple().to_string())
    .execute(fixture.pool())
    .await
    .expect("target");
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(source.mirror_id)
    .bind(source.target_id)
    .bind(format!("mirrors/aa/{}.git", source.mirror_id))
    .execute(fixture.pool())
    .await
    .expect("mirror");
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, created_at)
         values ($1, $2, 'fetch', 'succeeded', now())",
    )
    .bind(source.mirror_lifecycle_run_id)
    .bind(source.target_id)
    .execute(fixture.pool())
    .await
    .expect("mirror run");
    source
}

fn blob(digit: &str, media_type: &str) -> BlobRef {
    BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: digit.repeat(64),
        media_type: media_type.to_owned(),
        size_bytes: 1,
    }
}
