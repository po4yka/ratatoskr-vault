//! Off-host replication planning, health, and outage behavior.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::collections::HashMap;
use std::time::Duration;

use ratatoskr_vault::replication::{
    ReplicaHealth, ReplicaRequirement, ReplicationBacklogLimits, ReplicationCandidate,
    ReplicationUnit, ReplicationUnitOutcome, ReplicationWorker, ReplicationWorkerSettings,
    VerifiedPlacement, plan_replication_backlog, project_snapshot_health,
};
use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::ReplicaStore;
use ratatoskr_vault_core::config::ReplicaTargetConfig;
use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{ReplicaTargetObservation, SnapshotSource};
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn unreachable_replica_leaves_local_snapshot_and_verification_admissible() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = temporary_root();
    let store = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let bundle = publish_bytes(&store, &root, "bundle", b"verified-local-bundle");
    let manifest = publish_bytes(&store, &root, "manifest", b"verified-local-manifest");
    let snapshot_id = fixture
        .database
        .record_built_snapshot(
            healthy_source(&fixture).await,
            None,
            &bundle,
            &manifest,
            &"a".repeat(64),
            None,
        )
        .await
        .expect("snapshot evidence");
    let artifact_id: Uuid = sqlx::query_scalar(
        "select artifact_id from git_vault.snapshot_artifacts
         where snapshot_id = $1 and sha256_hash = decode($2, 'hex')",
    )
    .bind(snapshot_id)
    .bind(&bundle.sha256)
    .fetch_one(fixture.pool())
    .await
    .expect("bundle artifact identity");
    let target_id = Uuid::now_v7();
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id: target_id,
            name: "unreachable".to_owned(),
            endpoint_origin: "http://127.0.0.1:9".to_owned(),
            bucket: "vault-tests".to_owned(),
            key_prefix: "replicas".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("target inventory");
    let replica = ReplicaStore::new("unreachable".to_owned(), replica_target())
        .expect("bounded replica client");
    let worker = ReplicationWorker::new(
        fixture.database.clone(),
        store.clone(),
        HashMap::from([(target_id, replica)]),
        ReplicationWorkerSettings {
            max_concurrent: 1,
            lease: Duration::from_secs(5),
        },
    );
    let unit = ReplicationUnit {
        artifact_id,
        replica_target_id: target_id,
        blob: bundle.clone(),
    };

    let first = worker.run_unit(unit.clone()).await;
    assert_eq!(first, ReplicationUnitOutcome::Failed);
    store
        .verify(&bundle)
        .expect("remote failure cannot consume or rewrite local verification bytes");
    let second = worker.run_unit(unit).await;
    assert_eq!(second, ReplicationUnitOutcome::Failed);

    let failures: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.replication_attempts
         where artifact_id = $1 and outcome = 'failed'",
    )
    .bind(artifact_id)
    .fetch_one(fixture.pool())
    .await
    .expect("durable remote failures");
    assert_eq!(failures, 2, "a later bounded pass remains retryable");
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn shutdown_deadline_leaves_an_owned_attempt_recoverable() {
    let (endpoint, hanging_server) = hanging_endpoint().await;
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = temporary_root();
    let store = LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("local store");
    let bundle = publish_bytes(&store, &root, "bundle", b"shutdown-bundle");
    let manifest = publish_bytes(&store, &root, "manifest", b"shutdown-manifest");
    let snapshot_id = fixture
        .database
        .record_built_snapshot(
            healthy_source(&fixture).await,
            None,
            &bundle,
            &manifest,
            &"b".repeat(64),
            None,
        )
        .await
        .expect("snapshot evidence");
    let artifact_id: Uuid = sqlx::query_scalar(
        "select artifact_id from git_vault.snapshot_artifacts
         where snapshot_id = $1 and sha256_hash = decode($2, 'hex')",
    )
    .bind(snapshot_id)
    .bind(&bundle.sha256)
    .fetch_one(fixture.pool())
    .await
    .expect("bundle artifact identity");
    let target_id = Uuid::now_v7();
    fixture
        .database
        .observe_replica_target(&ReplicaTargetObservation {
            replica_target_id: target_id,
            name: "shutdown".to_owned(),
            endpoint_origin: endpoint.clone(),
            bucket: "vault-tests".to_owned(),
            key_prefix: "replicas".to_owned(),
            required: true,
            enabled: true,
        })
        .await
        .expect("target inventory");
    let worker = ReplicationWorker::new(
        fixture.database.clone(),
        store,
        HashMap::from([(
            target_id,
            ReplicaStore::new("shutdown".to_owned(), replica_target_at(&endpoint, 30))
                .expect("bounded replica client"),
        )]),
        ReplicationWorkerSettings {
            max_concurrent: 1,
            lease: Duration::from_secs(1),
        },
    );
    let run = worker.start_unit(ReplicationUnit {
        artifact_id,
        replica_target_id: target_id,
        blob: bundle,
    });
    wait_for_running_attempt(&fixture, artifact_id).await;

    let shutdown = tokio::time::timeout(Duration::from_millis(100), async {
        run.shutdown(Duration::from_millis(20)).await
    })
    .await;
    assert!(
        shutdown.is_ok(),
        "shutdown must enforce its finite deadline"
    );
    assert_eq!(
        shutdown.expect("bounded shutdown"),
        ReplicationUnitOutcome::Cancelled
    );
    let outcome: String = sqlx::query_scalar(
        "select outcome from git_vault.replication_attempts
         where artifact_id = $1 order by started_at desc limit 1",
    )
    .bind(artifact_id)
    .fetch_one(fixture.pool())
    .await
    .expect("owned attempt evidence");
    assert_eq!(outcome, "running", "cancellation must not invent success");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    fixture
        .database
        .claim_replication_attempt(
            artifact_id,
            target_id,
            Uuid::now_v7(),
            Duration::from_secs(1),
        )
        .await
        .expect("expired cancelled claim must be recoverable");
    hanging_server.abort();
    fixture.cleanup().await.expect("cleanup");
}

async fn hanging_endpoint() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        while let Ok((socket, _peer)) = listener.accept().await {
            tokio::spawn(async move {
                let _owned_socket = socket;
                std::future::pending::<()>().await;
            });
        }
    });
    (endpoint, server)
}

async fn wait_for_running_attempt(fixture: &TestDatabase, artifact_id: Uuid) {
    for _ in 0..100 {
        let running: i64 = sqlx::query_scalar(
            "select count(*) from git_vault.replication_attempts
             where artifact_id = $1 and outcome = 'running'",
        )
        .bind(artifact_id)
        .fetch_one(fixture.pool())
        .await
        .expect("attempt state");
        if running == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn backlog_admission_bounds_items_bytes_and_preserves_deferred_work() {
    let snapshot_id = Uuid::now_v7();
    let target_id = Uuid::now_v7();
    let largest = candidate(snapshot_id, target_id, 6, Some(30));
    let middle = candidate(snapshot_id, target_id, 5, Some(20));
    let oldest = candidate(snapshot_id, target_id, 4, None);
    let plan = plan_replication_backlog(
        vec![largest.clone(), middle.clone(), oldest.clone()],
        ReplicationBacklogLimits {
            max_items: 2,
            max_bytes: 10,
            max_concurrent: 1,
        },
    );

    assert_eq!(
        plan.admitted,
        vec![oldest, middle],
        "absent/oldest-first admission must honor item and aggregate byte ceilings"
    );
    assert_eq!(
        plan.admitted
            .iter()
            .map(|unit| unit.size_bytes)
            .sum::<u64>(),
        9
    );
    assert_eq!(plan.deferred, vec![largest]);
    assert_eq!(plan.max_concurrent, 1);
}

#[test]
fn required_replica_controls_snapshot_health_without_rewriting_local_pass() {
    let target = Uuid::now_v7();
    let bundle = Uuid::now_v7();
    let manifest = Uuid::now_v7();
    let health = project_snapshot_health(
        true,
        &[bundle, manifest],
        &[ReplicaRequirement {
            replica_target_id: target,
            required: true,
            enabled: true,
        }],
        &[VerifiedPlacement {
            artifact_id: bundle,
            replica_target_id: target,
        }],
    );

    assert!(
        health.local_verified,
        "the local pass remains immutable evidence"
    );
    assert_eq!(health.replica_health, ReplicaHealth::Degraded);
    assert!(
        !health.overall_healthy,
        "missing required manifest placement must degrade policy health"
    );
}

#[test]
fn optional_replica_failure_does_not_degrade_snapshot_health() {
    let required = Uuid::now_v7();
    let optional = Uuid::now_v7();
    let bundle = Uuid::now_v7();
    let manifest = Uuid::now_v7();
    let health = project_snapshot_health(
        true,
        &[bundle, manifest],
        &[
            ReplicaRequirement {
                replica_target_id: required,
                required: true,
                enabled: true,
            },
            ReplicaRequirement {
                replica_target_id: optional,
                required: false,
                enabled: true,
            },
        ],
        &[
            VerifiedPlacement {
                artifact_id: bundle,
                replica_target_id: required,
            },
            VerifiedPlacement {
                artifact_id: manifest,
                replica_target_id: required,
            },
        ],
    );

    assert_eq!(health.replica_health, ReplicaHealth::Complete);
    assert!(health.overall_healthy);
    assert!(health.local_verified);
}

fn candidate(
    snapshot_id: Uuid,
    replica_target_id: Uuid,
    size_bytes: u64,
    last_verified_at: Option<u64>,
) -> ReplicationCandidate {
    ReplicationCandidate {
        snapshot_id,
        artifact_id: Uuid::now_v7(),
        replica_target_id,
        size_bytes,
        last_verified_at,
    }
}

fn temporary_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("vault-replication-test-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("temporary root");
    root
}

fn publish_bytes(
    store: &LocalBlobStore,
    root: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> BlobRef {
    let source = root.join(name);
    std::fs::write(&source, bytes).expect("fixture bytes");
    let media_type = if name == "manifest" {
        "application/json"
    } else {
        "application/vnd.git.bundle"
    };
    let reference = store
        .reference_for_file(&source, media_type.to_owned())
        .expect("fixture reference");
    store
        .publish_file(&reference, &source)
        .expect("fixture publication");
    reference
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
    .expect("target fixture");
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
    .expect("mirror fixture");
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, created_at)
         values ($1, $2, 'fetch', 'succeeded', now())",
    )
    .bind(source.mirror_lifecycle_run_id)
    .bind(source.target_id)
    .execute(fixture.pool())
    .await
    .expect("mirror run fixture");
    source
}

fn replica_target() -> ReplicaTargetConfig {
    replica_target_at("http://127.0.0.1:9", 2)
}

fn replica_target_at(endpoint: &str, request_timeout_seconds: u64) -> ReplicaTargetConfig {
    ReplicaTargetConfig {
        endpoint: Url::parse(endpoint).expect("loopback endpoint"),
        bucket: "vault-tests".to_owned(),
        region: "us-east-1".to_owned(),
        key_prefix: "replicas".to_owned(),
        access_key: SecretString::from("fixture-access".to_owned()),
        secret_access_key: SecretString::from("fixture-secret".to_owned()),
        session_token: None,
        required: true,
        enabled: true,
        max_backlog_items: 16,
        max_backlog_bytes: 1_000_000,
        max_concurrent: 1,
        max_object_bytes: 1_000_000,
        connect_timeout_seconds: 1,
        attempt_timeout_seconds: 1,
        request_timeout_seconds,
    }
}
