//! Verification and restore report persistence against disposable `PostgreSQL`.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{
    EvidenceOutcome, SnapshotSource, StoredRestoreDrillReport, StoredVerificationReport,
};
use uuid::Uuid;

#[tokio::test]
async fn failed_drill_report_and_alert_fact_commit_atomically_and_remain_immutable() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let snapshot_id = persisted_snapshot(&fixture).await;
    let drill_id = Uuid::now_v7();
    let report = StoredRestoreDrillReport {
        drill_id,
        snapshot_id,
        manifest: blob("2", "application/json"),
        outcome: EvidenceOutcome::Failed,
        failure_class: Some("ref_mismatch".to_owned()),
        refs_matched: false,
        duration_millis: 27,
        stages: serde_json::json!([
            {"stage": "bundle_verify", "passed": true, "duration_millis": 5},
            {"stage": "ref_compare", "passed": false, "duration_millis": 1}
        ]),
        expected_ref_count: 2,
        observed_ref_count: 2,
        expected_ref_set_sha256: "3".repeat(64),
        observed_ref_set_sha256: "4".repeat(64),
        network_disabled: true,
        live_mirror_accessed: false,
    };

    fixture
        .database
        .record_restore_drill(&report)
        .await
        .expect("failed drill evidence must commit");

    let stored: (String, Option<String>, i64, i64, bool, bool) = sqlx::query_as(
        "select outcome, failure_class, duration_millis, observed_ref_count,
                network_disabled, live_mirror_accessed
         from git_vault.restore_drills where drill_id = $1",
    )
    .bind(drill_id)
    .fetch_one(fixture.pool())
    .await
    .expect("terminal drill report");
    assert_eq!(stored.0, "failed");
    assert_eq!(stored.1.as_deref(), Some("ref_mismatch"));
    assert_eq!(stored.2, 27);
    assert_eq!(stored.3, 2);
    assert!(stored.4);
    assert!(!stored.5);
    let event: (String, Uuid, String) = sqlx::query_as(
        "select event_type, aggregate_id, payload->>'failure_class'
         from git_vault.outbox where aggregate_id = $1",
    )
    .bind(drill_id)
    .fetch_one(fixture.pool())
    .await
    .expect("alert-worthy outbox fact");
    assert_eq!(event.0, "vault.restore.failed.v1");
    assert_eq!(event.1, drill_id);
    assert_eq!(event.2, "ref_mismatch");
    assert!(
        sqlx::query("update git_vault.restore_drills set outcome = 'passed' where drill_id = $1")
            .bind(drill_id)
            .execute(fixture.pool())
            .await
            .is_err(),
        "terminal evidence must reject mutation"
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn pass_and_failure_retries_append_distinct_terminal_reports() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let snapshot_id = persisted_snapshot(&fixture).await;
    let failed = drill_report(snapshot_id, EvidenceOutcome::Failed);
    let passed = drill_report(snapshot_id, EvidenceOutcome::Passed);
    let verification = StoredVerificationReport {
        verification_id: Uuid::now_v7(),
        snapshot_id,
        manifest: blob("2", "application/json"),
        outcome: EvidenceOutcome::Passed,
        failure_class: None,
        duration_millis: 11,
        stages: serde_json::json!([
            {"stage": "manifest_chain", "passed": true, "duration_millis": 3},
            {"stage": "bundle_hash", "passed": true, "duration_millis": 2}
        ]),
        checked_artifacts: serde_json::json!([blob("1", "application/vnd.git.bundle")]),
        expected_ref_count: 2,
        expected_ref_set_sha256: "3".repeat(64),
    };

    fixture
        .database
        .record_restore_drill(&failed)
        .await
        .expect("first failed attempt");
    fixture
        .database
        .record_restore_drill(&passed)
        .await
        .expect("later successful attempt");
    fixture
        .database
        .record_verification(&verification)
        .await
        .expect("successful verification");
    assert!(
        fixture
            .database
            .record_restore_drill(&failed)
            .await
            .is_err(),
        "retrying the same attempt id must not rewrite or duplicate evidence"
    );

    let reports: i64 =
        sqlx::query_scalar("select count(*) from git_vault.restore_drills where snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(fixture.pool())
            .await
            .expect("report count");
    assert_eq!(reports, 2);
    let schedule = fixture
        .database
        .verification_schedule_records()
        .await
        .expect("schedule inputs");
    let candidate = schedule
        .iter()
        .find(|candidate| candidate.snapshot_id == snapshot_id)
        .expect("snapshot schedule input");
    assert_eq!(candidate.bundle_size_bytes, 1);
    assert!(candidate.last_verified_at.is_some());
    assert!(candidate.last_drilled_at.is_some());
    fixture.cleanup().await.expect("cleanup");
}

fn drill_report(snapshot_id: Uuid, outcome: EvidenceOutcome) -> StoredRestoreDrillReport {
    let passed = outcome == EvidenceOutcome::Passed;
    StoredRestoreDrillReport {
        drill_id: Uuid::now_v7(),
        snapshot_id,
        manifest: blob("2", "application/json"),
        outcome,
        failure_class: (!passed).then(|| "ref_mismatch".to_owned()),
        refs_matched: passed,
        duration_millis: 27,
        stages: serde_json::json!([
            {"stage": "ref_compare", "passed": passed, "duration_millis": 1}
        ]),
        expected_ref_count: 2,
        observed_ref_count: 2,
        expected_ref_set_sha256: "3".repeat(64),
        observed_ref_set_sha256: if passed {
            "3".repeat(64)
        } else {
            "4".repeat(64)
        },
        network_disabled: true,
        live_mirror_accessed: false,
    }
}

async fn persisted_snapshot(fixture: &TestDatabase) -> Uuid {
    let source = healthy_source(fixture).await;
    fixture
        .database
        .record_built_snapshot(
            source,
            None,
            &blob("1", "application/vnd.git.bundle"),
            &blob("2", "application/json"),
            &"3".repeat(64),
        )
        .await
        .expect("snapshot evidence")
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
