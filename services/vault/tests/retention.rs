//! Local-first staged retention execution against disposable storage fixtures.

#![allow(clippy::expect_used, reason = "test assertions")]

#[path = "../../../crates/blobstore/tests/support/mod.rs"]
#[allow(
    dead_code,
    reason = "shared fixture exposes other blobstore test controls"
)]
mod s3_support;

use std::collections::HashMap;
use std::time::Duration;

use ratatoskr_vault::retention::{RetentionPlanOutcome, RetentionWorker, RetentionWorkerSettings};
use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::ReplicaStore;
use ratatoskr_vault_core::config::ReplicaTargetConfig;
use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::ReplicaTargetObservation;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{DeletionStageKind, StageClaimOutcome, StageClaimRequest};
use s3_support::S3Fixture;
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn eligible_snapshot_deletes_local_then_each_replica_with_journal_entries() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let s3 = S3Fixture::start().await;
    let root = temporary_root();
    let local = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let source = root.join("retention.bundle");
    let bytes = b"retention local then replica";
    std::fs::write(&source, bytes).expect("bundle fixture");
    let blob = local
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("blob reference");
    local
        .publish_file(&blob, &source)
        .expect("local publication");
    let replica_target_id = Uuid::now_v7();
    let replica = ReplicaStore::new("offsite".to_owned(), replica_target(s3.endpoint()))
        .expect("replica store");
    let remote = replica
        .upload_and_verify(&blob, &source)
        .await
        .expect("verified remote fixture");
    let seeded = seed_plan(&fixture, replica_target_id, &blob, &remote.object_key, true).await;
    let mirror_path = root.join(format!("mirrors/12/{}.git", seeded.mirror));
    std::fs::create_dir_all(mirror_path.join("objects")).expect("mirror fixture");
    std::fs::write(mirror_path.join("HEAD"), b"ref: refs/heads/main\n").expect("mirror marker");
    let worker = RetentionWorker::new(
        fixture.database.clone(),
        local.clone(),
        HashMap::from([(replica_target_id, replica.clone())]),
        RetentionWorkerSettings {
            lease: Duration::from_mins(1),
            mirror_root: root.clone(),
        },
    );

    assert_eq!(
        worker.run_plan(seeded.plan).await,
        RetentionPlanOutcome::Completed
    );
    assert!(local.resolve(&blob).is_err(), "local bytes must be absent");
    assert!(!mirror_path.exists(), "target mirror must be absent");
    let terminal = replica
        .delete_verified(
            &blob,
            &ratatoskr_vault_blobstore::replica::ReplicaPlacement {
                target: "offsite".to_owned(),
                object_key: remote.object_key,
                size_bytes: blob.size_bytes,
                sha256: blob.sha256.clone(),
            },
        )
        .await;
    assert!(matches!(
        terminal,
        Ok(ratatoskr_vault_blobstore::replica::ReplicaDeleteOutcome::AlreadyAbsent)
    ));
    let stages: Vec<(String, String)> = sqlx::query_as(
        "select stage_kind, outcome from git_vault.deletion_stage_attempts
         where plan_id = $1 order by ordinal",
    )
    .bind(seeded.plan)
    .fetch_all(fixture.pool())
    .await
    .expect("stage journal");
    assert_eq!(
        stages,
        vec![
            ("local".to_owned(), "succeeded".to_owned()),
            ("mirror_local".to_owned(), "succeeded".to_owned()),
            ("replica".to_owned(), "succeeded".to_owned()),
        ]
    );
    let (plan_status, snapshot_status, target_status, stage_audits): (String, String, String, i64) =
        sqlx::query_as(
            "select plans.status, snapshots.status, targets.status,
                    (select count(*) from git_vault.retention_audit audit
                     where audit.plan_id = plans.plan_id and audit.event_kind = 'stage')
             from git_vault.deletion_plans plans
             join git_vault.snapshots on snapshots.snapshot_id = plans.snapshot_id
             join git_vault.targets on targets.target_id = plans.target_id
             where plans.plan_id = $1",
        )
        .bind(seeded.plan)
        .fetch_one(fixture.pool())
        .await
        .expect("terminal projections");
    assert_eq!(plan_status, "completed");
    assert_eq!(snapshot_status, "deleted");
    assert_eq!(target_status, "deleted");
    assert!(
        stage_audits >= 4,
        "claims and terminal stages must be audited"
    );

    fixture.cleanup().await.expect("cleanup");
    let _ignored = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn shared_blob_is_retained_while_another_snapshot_references_it() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let s3 = S3Fixture::start().await;
    let root = temporary_root();
    let local = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let source = root.join("shared.bundle");
    std::fs::write(&source, b"shared retained bytes").expect("bundle fixture");
    let blob = local
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("blob reference");
    local
        .publish_file(&blob, &source)
        .expect("local publication");
    let replica_target_id = Uuid::now_v7();
    let replica = ReplicaStore::new("offsite".to_owned(), replica_target(s3.endpoint()))
        .expect("replica store");
    let remote = replica
        .upload_and_verify(&blob, &source)
        .await
        .expect("verified remote fixture");
    let seeded = seed_plan(
        &fixture,
        replica_target_id,
        &blob,
        &remote.object_key,
        false,
    )
    .await;
    seed_retained_reference(
        &fixture,
        &seeded,
        replica_target_id,
        &blob,
        &remote.object_key,
    )
    .await;
    let requests_before = s3.requests().len();
    let worker = RetentionWorker::new(
        fixture.database.clone(),
        local.clone(),
        HashMap::from([(replica_target_id, replica.clone())]),
        RetentionWorkerSettings {
            lease: Duration::from_mins(1),
            mirror_root: root.clone(),
        },
    );

    assert_eq!(
        worker.run_plan(seeded.plan).await,
        RetentionPlanOutcome::Completed
    );
    local.verify(&blob).expect("shared local bytes must remain");
    let scratch = root.join("shared-remote-download");
    replica
        .download_verified(&blob, &scratch)
        .await
        .expect("shared remote bytes must remain");
    let stages: Vec<String> = sqlx::query_scalar(
        "select outcome from git_vault.deletion_stage_attempts
         where plan_id = $1 order by ordinal",
    )
    .bind(seeded.plan)
    .fetch_all(fixture.pool())
    .await
    .expect("shared stage journal");
    assert_eq!(
        stages,
        vec![
            "shared_reference_retained".to_owned(),
            "shared_reference_retained".to_owned(),
        ]
    );
    assert!(
        s3.requests()
            .into_iter()
            .skip(requests_before)
            .all(|request| !request.contains("?delete")),
        "shared replica must not receive DELETE"
    );
    fixture.cleanup().await.expect("cleanup");
    let _ignored = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn expired_stage_lease_reconciles_effect_without_repeating_it() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let s3 = S3Fixture::start().await;
    let root = temporary_root();
    let local = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let source = root.join("crash.bundle");
    std::fs::write(&source, b"deleted before journal commit").expect("bundle fixture");
    let blob = local
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("blob reference");
    local
        .publish_file(&blob, &source)
        .expect("local publication");
    let replica_target_id = Uuid::now_v7();
    let replica = ReplicaStore::new("offsite".to_owned(), replica_target(s3.endpoint()))
        .expect("replica store");
    let remote = replica
        .upload_and_verify(&blob, &source)
        .await
        .expect("verified remote fixture");
    let seeded = seed_plan(
        &fixture,
        replica_target_id,
        &blob,
        &remote.object_key,
        false,
    )
    .await;
    let first_claim = fixture
        .database
        .claim_deletion_stage(&StageClaimRequest {
            plan_id: seeded.plan,
            kind: DeletionStageKind::Local,
            stage_key: format!("local:{}", seeded.artifact),
            artifact_id: Some(seeded.artifact),
            replica_target_id: None,
            placement_id: None,
            lease_owner: Uuid::now_v7(),
            lease_seconds: 1,
        })
        .await
        .expect("first local claim");
    assert!(matches!(first_claim, StageClaimOutcome::Claimed { .. }));
    local
        .delete_verified_blob(&blob)
        .expect("external effect occurs before simulated crash");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let worker = RetentionWorker::new(
        fixture.database.clone(),
        local,
        HashMap::from([(replica_target_id, replica)]),
        RetentionWorkerSettings {
            lease: Duration::from_mins(1),
            mirror_root: root.clone(),
        },
    );

    assert_eq!(
        worker.run_plan(seeded.plan).await,
        RetentionPlanOutcome::Completed
    );
    let attempts: Vec<String> = sqlx::query_scalar(
        "select outcome from git_vault.deletion_stage_attempts
         where plan_id = $1 and stage_kind = 'local' order by ordinal",
    )
    .bind(seeded.plan)
    .fetch_all(fixture.pool())
    .await
    .expect("recovered local attempts");
    assert_eq!(
        attempts,
        vec!["abandoned".to_owned(), "succeeded".to_owned()]
    );

    fixture.cleanup().await.expect("cleanup");
    let _ignored = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn failed_replica_is_journaled_and_resumable() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let s3 = S3Fixture::start().await;
    let root = temporary_root();
    let local = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let source = root.join("retry.bundle");
    std::fs::write(&source, b"retry replica deletion").expect("bundle fixture");
    let blob = local
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("blob reference");
    local
        .publish_file(&blob, &source)
        .expect("local publication");
    let replica_target_id = Uuid::now_v7();
    let replica = ReplicaStore::new("offsite".to_owned(), replica_target(s3.endpoint()))
        .expect("replica store");
    let remote = replica
        .upload_and_verify(&blob, &source)
        .await
        .expect("verified remote fixture");
    let seeded = seed_plan(
        &fixture,
        replica_target_id,
        &blob,
        &remote.object_key,
        false,
    )
    .await;
    let worker = RetentionWorker::new(
        fixture.database.clone(),
        local,
        HashMap::from([(replica_target_id, replica)]),
        RetentionWorkerSettings {
            lease: Duration::from_mins(1),
            mirror_root: root.clone(),
        },
    );
    s3.retain_next_delete();

    assert_eq!(
        worker.run_plan(seeded.plan).await,
        RetentionPlanOutcome::Failed
    );
    let failed: Vec<(String, Option<String>)> = sqlx::query_as(
        "select outcome, failure_class from git_vault.deletion_stage_attempts
         where plan_id = $1 and stage_kind = 'replica' order by ordinal",
    )
    .bind(seeded.plan)
    .fetch_all(fixture.pool())
    .await
    .expect("failed replica journal");
    assert_eq!(
        failed,
        vec![("failed".to_owned(), Some("remote_verification".to_owned()))]
    );

    assert_eq!(
        worker.run_plan(seeded.plan).await,
        RetentionPlanOutcome::Completed
    );
    let outcomes: Vec<String> = sqlx::query_scalar(
        "select outcome from git_vault.deletion_stage_attempts
         where plan_id = $1 and stage_kind = 'replica' order by ordinal",
    )
    .bind(seeded.plan)
    .fetch_all(fixture.pool())
    .await
    .expect("resumed replica journal");
    assert_eq!(outcomes, vec!["failed".to_owned(), "succeeded".to_owned()]);

    fixture.cleanup().await.expect("cleanup");
    let _ignored = std::fs::remove_dir_all(root);
}

async fn seed_retained_reference(
    fixture: &TestDatabase,
    seeded: &SeededPlan,
    replica_target_id: Uuid,
    blob: &BlobRef,
    object_key: &str,
) {
    let snapshot = Uuid::now_v7();
    let artifact = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.snapshots
             (snapshot_id, target_id, mirror_id, mirror_lifecycle_run_id, format, status,
              refs_hash, created_at)
         values ($1, $2, $3, $4, 'git_bundle', 'restorable', $5, now())",
    )
    .bind(snapshot)
    .bind(seeded.target)
    .bind(seeded.mirror)
    .bind(seeded.run)
    .bind(vec![10_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("retained snapshot");
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', decode($3, 'hex'), $4, 'sha256', $5, $6, now())",
    )
    .bind(artifact)
    .bind(snapshot)
    .bind(&blob.sha256)
    .bind(&blob.owner)
    .bind(&blob.media_type)
    .bind(i64::try_from(blob.size_bytes).expect("fixture size"))
    .execute(fixture.pool())
    .await
    .expect("retained artifact");
    let owner = Uuid::now_v7();
    let attempt = fixture
        .database
        .claim_replication_attempt(artifact, replica_target_id, owner, Duration::from_mins(1))
        .await
        .expect("retained placement claim");
    fixture
        .database
        .record_verified_replica(attempt, owner, object_key, blob)
        .await
        .expect("retained placement");
}

struct SeededPlan {
    plan: Uuid,
    target: Uuid,
    artifact: Uuid,
    mirror: Uuid,
    run: Uuid,
}

async fn seed_plan(
    fixture: &TestDatabase,
    replica_target_id: Uuid,
    blob: &BlobRef,
    object_key: &str,
    target_inactive: bool,
) -> SeededPlan {
    let storage = seed_storage(fixture, blob, target_inactive).await;
    seed_placement(
        fixture,
        storage.artifact,
        replica_target_id,
        object_key,
        blob,
    )
    .await;
    let plan = seed_retention_intent(fixture, &storage, blob.size_bytes, target_inactive).await;
    SeededPlan {
        plan,
        target: storage.target,
        artifact: storage.artifact,
        mirror: storage.mirror,
        run: storage.run,
    }
}

struct SeedStorage {
    target: Uuid,
    snapshot: Uuid,
    artifact: Uuid,
    mirror: Uuid,
    run: Uuid,
}

async fn seed_storage(
    fixture: &TestDatabase,
    blob: &BlobRef,
    target_inactive: bool,
) -> SeedStorage {
    let target_id = Uuid::now_v7();
    let mirror_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let snapshot_id = Uuid::now_v7();
    let artifact_id = Uuid::now_v7();
    let target_status = if target_inactive {
        "excluded"
    } else {
        "healthy"
    };
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, $3, now(), now())",
    )
    .bind(target_id)
    .bind(format!("retention-worker-{target_id}"))
    .bind(target_status)
    .execute(fixture.pool())
    .await
    .expect("target");
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(mirror_id)
    .bind(target_id)
    .bind(format!("mirrors/12/{mirror_id}.git"))
    .execute(fixture.pool())
    .await
    .expect("mirror");
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, created_at)
         values ($1, $2, 'fetch', 'succeeded', now())",
    )
    .bind(run_id)
    .bind(target_id)
    .execute(fixture.pool())
    .await
    .expect("run");
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
    .bind(vec![8_u8; 32])
    .execute(fixture.pool())
    .await
    .expect("snapshot");
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, 'git_bundle', decode($3, 'hex'), $4, 'sha256', $5, $6, now())",
    )
    .bind(artifact_id)
    .bind(snapshot_id)
    .bind(&blob.sha256)
    .bind(&blob.owner)
    .bind(&blob.media_type)
    .bind(i64::try_from(blob.size_bytes).expect("fixture size"))
    .execute(fixture.pool())
    .await
    .expect("artifact");
    SeedStorage {
        target: target_id,
        snapshot: snapshot_id,
        artifact: artifact_id,
        mirror: mirror_id,
        run: run_id,
    }
}

async fn seed_placement(
    fixture: &TestDatabase,
    artifact_id: Uuid,
    replica_target_id: Uuid,
    object_key: &str,
    blob: &BlobRef,
) {
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id,
            name: "offsite".to_owned(),
            endpoint_origin: "http://127.0.0.1:1".to_owned(),
            bucket: "vault-fixtures".to_owned(),
            key_prefix: "test".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("replica target");
    let attempt_id = fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            replica_target_id,
            Uuid::from_u128(99),
            Duration::from_mins(1),
        )
        .await
        .expect("replication attempt");
    fixture
        .database
        .record_verified_replica(attempt_id, Uuid::from_u128(99), object_key, blob)
        .await
        .expect("placement");
}

async fn seed_retention_intent(
    fixture: &TestDatabase,
    storage: &SeedStorage,
    size_bytes: u64,
    target_inactive: bool,
) -> Uuid {
    let policy_id = Uuid::now_v7();
    let evaluation_id = Uuid::now_v7();
    let tombstone_id = Uuid::now_v7();
    let plan_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.retention_policies
             (policy_id, name, minimum_age_seconds, grace_seconds,
              keep_last_restorable, created_at)
         values ($1, $2, 0, 60, 1, now())",
    )
    .bind(policy_id)
    .bind(format!("worker-{}", storage.target))
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
    .bind(storage.target)
    .bind(policy_id)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("evaluation");
    if target_inactive {
        sqlx::query(
            "insert into git_vault.tombstones
             (tombstone_id, target_id, governing_policy_revision, reason, was_pinned,
              correlation_id, recorded_at, not_before)
         values ($1, $2, 2, 'policy_inactive', false, $3,
                 now() - interval '2 hours', now() - interval '1 hour')",
        )
        .bind(tombstone_id)
        .bind(storage.target)
        .bind(Uuid::now_v7())
        .execute(fixture.pool())
        .await
        .expect("tombstone");
    }
    let plan_tombstone = target_inactive.then_some(tombstone_id);
    let plan_reason = if target_inactive {
        "target_inactive"
    } else {
        "ordinary_retention"
    };
    sqlx::query(
        "insert into git_vault.deletion_plans
             (plan_id, evaluation_id, target_id, snapshot_id, tombstone_id, reason,
              automatic, tombstoned_at, not_before, estimated_bytes, correlation_id)
         values ($1, $2, $3, $4, $5, $6, true,
                 now() - interval '2 hours', now() - interval '1 hour', $7, $8)",
    )
    .bind(plan_id)
    .bind(evaluation_id)
    .bind(storage.target)
    .bind(storage.snapshot)
    .bind(plan_tombstone)
    .bind(plan_reason)
    .bind(i64::try_from(size_bytes).expect("fixture size"))
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("plan");
    plan_id
}

fn replica_target(endpoint: &str) -> ReplicaTargetConfig {
    ReplicaTargetConfig {
        endpoint: Url::parse(endpoint).expect("fixture endpoint"),
        bucket: "vault-fixtures".to_owned(),
        region: "fixture-1".to_owned(),
        key_prefix: "test".to_owned(),
        enabled: true,
        required: true,
        access_key: SecretString::from("fixture-access"),
        secret_access_key: SecretString::from("fixture-secret"),
        session_token: None,
        connect_timeout_seconds: 2,
        request_timeout_seconds: 10,
        attempt_timeout_seconds: 5,
        max_object_bytes: 1024 * 1024,
        max_backlog_items: 8,
        max_backlog_bytes: 8 * 1024 * 1024,
        max_concurrent: 2,
    }
}

fn temporary_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vault-retention-test-{}", Uuid::now_v7()))
}
