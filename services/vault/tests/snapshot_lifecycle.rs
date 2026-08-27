//! Full-bundle snapshot acceptance tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions"
)]

use std::path::{Path, PathBuf};

use ratatoskr_vault::snapshot_lifecycle::{SnapshotLifecycle, SnapshotRequest, SnapshotSettings};
use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_core::snapshot::ManifestSigningKey;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

#[tokio::test]
async fn healthy_fixture_mirror_produces_complete_immutable_restorable_bundle_evidence() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = temporary_root();
    let mirror = create_fixture_mirror(&root);
    let request = healthy_request(&fixture).await;
    let store = LocalBlobStore::new(root.join("blobs"), 1_000_000).unwrap();
    let lifecycle = SnapshotLifecycle::new(
        fixture.database.clone(),
        SnapshotSettings {
            work_root: root.join("work"),
            git_binary: git_binary(),
            max_bytes: 1_000_000,
            manifest_signer: fixture_signer(),
        },
        store.clone(),
    )
    .unwrap();
    let snapshot = lifecycle
        .snapshot(request, &mirror, None)
        .await
        .expect("snapshot must publish");

    assert_eq!(snapshot.evidence.schema_version, 1);
    assert_eq!(snapshot.evidence.refs.len(), 2);
    assert_eq!(snapshot.evidence.bundles, vec![snapshot.bundle.clone()]);
    assert_eq!(snapshot.bundle.owner, "ratatoskr-vault");
    assert_eq!(snapshot.manifest.media_type, "application/json");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.resolve(&snapshot.manifest).unwrap()).unwrap())
            .unwrap();
    assert_eq!(manifest["refs"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["bundles"][0]["sha256"], snapshot.bundle.sha256);
    assert_eq!(
        snapshot
            .evidence
            .verify_signature(&[fixture_signer().verification_key()]),
        Ok(())
    );

    let restored = root.join("restored");
    let bundle_path = store.resolve(&snapshot.bundle).unwrap();
    git(
        &[
            "clone",
            "-q",
            bundle_path.to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        None,
    );
    let refs = git_output(&["show-ref"], Some(&restored));
    assert!(refs.contains("refs/heads/main"));
    assert!(refs.contains("refs/tags/v1"));
    let records: i64 =
        sqlx::query_scalar("select count(*) from git_vault.snapshots where snapshot_id = $1")
            .bind(snapshot.snapshot_id)
            .fetch_one(fixture.pool())
            .await
            .unwrap();
    assert_eq!(records, 1);

    std::fs::remove_dir_all(root).unwrap();
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn publication_failure_preserves_the_last_healthy_mirror_observation() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = temporary_root();
    let mirror = create_fixture_mirror(&root);
    let request = healthy_request(&fixture).await;
    let lifecycle = SnapshotLifecycle::new(
        fixture.database.clone(),
        SnapshotSettings {
            work_root: root.join("work"),
            git_binary: git_binary(),
            max_bytes: 1_000_000,
            manifest_signer: fixture_signer(),
        },
        LocalBlobStore::new(root.join("blobs"), 1).unwrap(),
    )
    .unwrap();
    let result = lifecycle.snapshot(request, &mirror, None).await;
    assert!(
        result.is_err(),
        "a BlobStore refusal must fail the snapshot"
    );
    let snapshot_count: i64 = sqlx::query_scalar("select count(*) from git_vault.snapshots")
        .fetch_one(fixture.pool())
        .await
        .unwrap();
    assert_eq!(snapshot_count, 0, "failed publication claims no snapshot");
    let mirror_status: String =
        sqlx::query_scalar("select status from git_vault.mirrors where mirror_id = $1")
            .bind(request.mirror_id)
            .fetch_one(fixture.pool())
            .await
            .unwrap();
    assert_eq!(
        mirror_status, "ready",
        "last healthy mirror evidence is retained"
    );

    std::fs::remove_dir_all(root).unwrap();
    fixture.cleanup().await.expect("cleanup");
}

async fn healthy_request(fixture: &TestDatabase) -> SnapshotRequest {
    let request = SnapshotRequest {
        target_id: Uuid::now_v7(),
        mirror_id: Uuid::now_v7(),
        mirror_lifecycle_run_id: Uuid::now_v7(),
    };
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'ready', now(), now())",
    )
    .bind(request.target_id)
    .bind(Uuid::now_v7().simple().to_string())
    .execute(fixture.pool())
    .await
    .unwrap();
    sqlx::query(
        "insert into git_vault.mirrors
             (mirror_id, target_id, status, storage_path, fsck_result, created_at, updated_at)
         values ($1, $2, 'ready', $3, 'ok', now(), now())",
    )
    .bind(request.mirror_id)
    .bind(request.target_id)
    .bind(format!("mirrors/aa/{}.git", request.mirror_id))
    .execute(fixture.pool())
    .await
    .unwrap();
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, created_at)
         values ($1, $2, 'fetch', 'succeeded', now())",
    )
    .bind(request.mirror_lifecycle_run_id)
    .bind(request.target_id)
    .execute(fixture.pool())
    .await
    .unwrap();
    request
}

fn create_fixture_mirror(root: &Path) -> PathBuf {
    let source = root.join("source");
    let mirror = root.join("mirror.git");
    git(
        &[
            "init",
            "-q",
            "--initial-branch=main",
            source.to_str().unwrap(),
        ],
        None,
    );
    std::fs::write(source.join("tracked.txt"), "snapshot\n").unwrap();
    git(&["add", "tracked.txt"], Some(&source));
    git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
        Some(&source),
    );
    git(&["tag", "v1"], Some(&source));
    git(
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            mirror.to_str().unwrap(),
        ],
        None,
    );
    mirror
}

fn git_binary() -> PathBuf {
    PathBuf::from("/usr/bin/git")
}

fn fixture_signer() -> std::sync::Arc<ManifestSigningKey> {
    std::sync::Arc::new(
        ManifestSigningKey::from_seed([17; 32]).expect("fixture signing key must load"),
    )
}

fn git(args: &[&str], cwd: Option<&Path>) {
    assert!(
        std::process::Command::new(git_binary())
            .args(args)
            .current_dir(cwd.unwrap_or_else(|| Path::new(".")))
            .status()
            .unwrap()
            .success()
    );
}

fn git_output(args: &[&str], cwd: Option<&Path>) -> String {
    String::from_utf8(
        std::process::Command::new(git_binary())
            .args(args)
            .current_dir(cwd.unwrap())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
}

fn temporary_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vault-snapshot-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    root
}
