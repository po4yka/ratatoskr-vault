//! Transactional and immutable LFS terminal evidence.

#![allow(clippy::expect_used, reason = "test assertions")]

use ratatoskr_vault_core::snapshot::{BlobRef, LfsEvidence, LfsObjectEvidence};
use ratatoskr_vault_persistence::LfsCollectionTerminal;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

#[tokio::test]
async fn complete_lfs_evidence_requires_the_exact_canonical_object_set() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let (target_id, run_id) = successful_mirror(&fixture).await;
    let evidence = complete_evidence();
    fixture
        .database
        .record_lfs_collection(
            target_id,
            run_id,
            LfsCollectionTerminal::Complete(&evidence),
        )
        .await
        .expect("canonical evidence");
    let mut inconsistent = evidence;
    inconsistent.total_bytes += 1;
    assert!(
        fixture
            .database
            .record_lfs_collection(
                target_id,
                run_id,
                LfsCollectionTerminal::Complete(&inconsistent),
            )
            .await
            .is_err(),
        "count/byte/digest disagreement is rejected before a row is written"
    );
    let rows: i64 =
        sqlx::query_scalar("select count(*) from git_vault.collector_runs where target_id = $1")
            .bind(target_id)
            .fetch_one(fixture.pool())
            .await
            .expect("collector count");
    assert_eq!(rows, 1);
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn terminal_lfs_results_cannot_be_rewritten() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let (target_id, run_id) = successful_mirror(&fixture).await;
    let collector_run_id = fixture
        .database
        .record_lfs_collection(
            target_id,
            run_id,
            LfsCollectionTerminal::Failed {
                failure_class: "vault.lfs.incomplete",
            },
        )
        .await
        .expect("failed evidence");
    let rewrite = sqlx::query(
        "update git_vault.collector_runs set outcome = 'complete', failure_class = null
         where collector_run_id = $1",
    )
    .bind(collector_run_id)
    .execute(fixture.pool())
    .await;
    assert!(
        rewrite.is_err(),
        "terminal collector evidence is append-only"
    );
    fixture.cleanup().await.expect("cleanup");
}

fn complete_evidence() -> LfsEvidence {
    LfsEvidence::new(
        "git-lfs/fixture".to_owned(),
        vec![LfsObjectEvidence {
            oid: "a".repeat(64),
            blob: BlobRef {
                owner: "ratatoskr-vault".to_owned(),
                sha256: "a".repeat(64),
                media_type: "application/octet-stream".to_owned(),
                size_bytes: 7,
            },
        }],
    )
}

async fn successful_mirror(fixture: &TestDatabase) -> (Uuid, Uuid) {
    let target_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'ready', now(), now())",
    )
    .bind(target_id)
    .bind(Uuid::now_v7().simple().to_string())
    .execute(fixture.pool())
    .await
    .expect("target");
    sqlx::query(
        "insert into git_vault.mirror_lifecycle_runs
             (run_id, target_id, operation, outcome, object_count, bytes_on_disk, created_at)
         values ($1, $2, 'clone', 'succeeded', 1, 1, now())",
    )
    .bind(run_id)
    .bind(target_id)
    .execute(fixture.pool())
    .await
    .expect("mirror run");
    (target_id, run_id)
}
