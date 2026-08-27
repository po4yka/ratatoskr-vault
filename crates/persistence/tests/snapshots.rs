//! Snapshot evidence persistence tests against disposable `PostgreSQL`.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::SnapshotSource;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

#[tokio::test]
async fn snapshot_records_link_one_manifest_and_bundle_refs_to_the_successful_mirror_run() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let source = healthy_source(&fixture).await;
    let bundle = blob("1", "application/vnd.git.bundle");
    let manifest = blob("2", "application/json");
    let snapshot_id = fixture
        .database
        .record_built_snapshot(source, None, &bundle, &manifest, &"3".repeat(64), None)
        .await
        .expect("built snapshot record");

    let record: (Uuid, Uuid, Uuid, Uuid, Vec<u8>, String, Vec<u8>) = sqlx::query_as(
        "select snapshots.snapshot_id, snapshots.target_id, snapshots.mirror_id,
                snapshots.mirror_lifecycle_run_id, snapshots.refs_hash,
                manifests.blob_owner, manifests.manifest_hash
         from git_vault.snapshots
         join git_vault.manifests using (snapshot_id)
         where snapshots.snapshot_id = $1",
    )
    .bind(snapshot_id)
    .fetch_one(fixture.pool())
    .await
    .expect("linked snapshot evidence");
    assert_eq!(record.0, snapshot_id);
    assert_eq!(record.1, source.target_id);
    assert_eq!(record.2, source.mirror_id);
    assert_eq!(record.3, source.mirror_lifecycle_run_id);
    assert_eq!(record.4, vec![0x33; 32]);
    assert_eq!(record.5, "ratatoskr-vault");
    assert_eq!(record.6, vec![0x22; 32]);

    let duplicate_manifest = sqlx::query(
        "insert into git_vault.manifests
             (manifest_id, snapshot_id, manifest_hash, blob_owner, digest_algorithm, media_type,
              size_bytes, created_at)
         values ($1, $2, decode(repeat('4', 64), 'hex'), 'ratatoskr-vault', 'sha256',
                 'application/json', 1, now())",
    )
    .bind(Uuid::now_v7())
    .bind(snapshot_id)
    .execute(fixture.pool())
    .await;
    assert!(
        duplicate_manifest.is_err(),
        "a snapshot must not name more than one manifest"
    );

    let parent = fixture
        .database
        .snapshot_parent(source)
        .await
        .expect("latest manifest lookup")
        .expect("first snapshot is the parent of a later one");
    assert_eq!(parent.snapshot_id, snapshot_id);
    assert_eq!(parent.manifest, manifest);
    fixture.cleanup().await.expect("cleanup");
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
    .expect("successful mirror run");
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
