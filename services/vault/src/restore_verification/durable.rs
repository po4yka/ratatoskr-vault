//! Durable composition of bounded verification work and append-only evidence.

use std::time::Duration;

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::{
    Database, EvidenceOutcome, StoredRestoreDrillReport, StoredRestoreSource,
    StoredVerificationReport,
};
use uuid::Uuid;

use super::{
    ArtifactVerifier, ReportOutcome, RestoreDrill, RestoreDrillReport, ScheduleCandidate,
    SchedulePlan, StageReport, VerificationFailure, VerificationPolicy, VerificationReport,
    plan_due_snapshots,
};

/// Persisted outcome of one verification attempt and its conditional restore drill.
#[derive(Debug, Clone)]
pub struct VerificationExecution {
    /// Terminal stored-byte and manifest result.
    pub verification: VerificationReport,
    /// Terminal drill result, absent when verification failed closed.
    pub drill: Option<RestoreDrillReport>,
}

/// Joins filesystem/Git observation to short append-only evidence transactions.
#[derive(Debug, Clone)]
pub struct DurableVerification {
    database: Database,
    verifier: ArtifactVerifier,
    drill: RestoreDrill,
}

impl DurableVerification {
    /// Creates the service boundary without starting work.
    #[must_use]
    pub const fn new(database: Database, verifier: ArtifactVerifier, drill: RestoreDrill) -> Self {
        Self {
            database,
            verifier,
            drill,
        }
    }

    /// Re-verifies one snapshot, persists its result, and drills only trusted stored bytes.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when terminal evidence and its failure fact cannot commit.
    pub async fn run(
        &self,
        snapshot_id: Uuid,
        manifest: BlobRef,
        execute_drill: bool,
    ) -> Result<VerificationExecution, VaultError> {
        let verification = self.verifier.verify(snapshot_id, manifest);
        self.database
            .record_verification(&stored_verification(&verification))
            .await?;
        let drill = if execute_drill && verification.outcome == ReportOutcome::Passed {
            let report = self.drill.run(&verification).await;
            self.database
                .record_restore_drill(&stored_drill(&report))
                .await?;
            Some(report)
        } else {
            None
        };
        Ok(VerificationExecution {
            verification,
            drill,
        })
    }

    /// Executes one already-admitted finite schedule plan in deterministic order.
    ///
    /// Each item receives a distinct terminal result. A failure does not erase or prevent later
    /// attempts in the same bounded plan.
    pub async fn run_plan(
        &self,
        plan: &SchedulePlan,
    ) -> Vec<(Uuid, Result<VerificationExecution, VaultError>)> {
        let mut results = Vec::with_capacity(plan.selected.len());
        for snapshot_id in &plan.selected {
            let result = match self.database.snapshot_manifest(*snapshot_id).await {
                Ok(manifest) => {
                    self.run(
                        *snapshot_id,
                        manifest,
                        plan.drill_selected.contains(snapshot_id),
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            results.push((*snapshot_id, result));
        }
        results
    }
}

/// Plans one finite scheduler pass from successful append-only database evidence.
///
/// # Errors
///
/// Returns a persistence error or rejects a non-positive policy.
pub async fn plan_due_from_database(
    database: &Database,
    now_unix_seconds: u64,
    policy: VerificationPolicy,
) -> Result<SchedulePlan, String> {
    let candidates = database
        .verification_schedule_records()
        .await
        .map_err(|error| format!("verification schedule evidence read failed: {error:?}"))?
        .into_iter()
        .map(|record| ScheduleCandidate {
            snapshot_id: record.snapshot_id,
            bundle_size_bytes: record.bundle_size_bytes,
            last_verified_at: record.last_verified_at,
            last_drilled_at: record.last_drilled_at,
        })
        .collect();
    plan_due_snapshots(now_unix_seconds, policy, candidates).map_err(|error| error.to_string())
}

fn stored_verification(report: &VerificationReport) -> StoredVerificationReport {
    StoredVerificationReport {
        verification_id: report.verification_id,
        snapshot_id: report.snapshot_id,
        manifest: report.manifest.clone(),
        outcome: evidence_outcome(report.outcome),
        failure_class: report.failure.map(failure_code).map(str::to_owned),
        duration_millis: duration_millis(report.duration),
        stages: stages_json(&report.stages),
        checked_artifacts: serde_json::json!(report.checked_artifacts),
        expected_ref_count: u64::try_from(report.expected_ref_count).unwrap_or(u64::MAX),
        expected_ref_set_sha256: report.expected_ref_set_sha256.clone(),
    }
}

fn stored_drill(report: &RestoreDrillReport) -> StoredRestoreDrillReport {
    StoredRestoreDrillReport {
        drill_id: report.drill_id,
        snapshot_id: report.snapshot_id,
        manifest: report.manifest.clone(),
        source: match report.source {
            super::RestoreSource::Local => StoredRestoreSource::Local,
            super::RestoreSource::Replica { replica_target_id } => {
                StoredRestoreSource::Replica(replica_target_id)
            }
        },
        outcome: evidence_outcome(report.outcome),
        failure_class: report.failure.map(failure_code).map(str::to_owned),
        refs_matched: report.expected_ref_count == report.observed_ref_count
            && report.expected_ref_set_sha256 == report.observed_ref_set_sha256,
        duration_millis: duration_millis(report.duration),
        stages: stages_json(&report.stages),
        expected_ref_count: u64::try_from(report.expected_ref_count).unwrap_or(u64::MAX),
        observed_ref_count: u64::try_from(report.observed_ref_count).unwrap_or(u64::MAX),
        expected_ref_set_sha256: report.expected_ref_set_sha256.clone(),
        observed_ref_set_sha256: report.observed_ref_set_sha256.clone(),
        lfs_restored: report.lfs_restored,
        expected_lfs_object_count: report
            .expected_lfs_object_count
            .map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
        observed_lfs_object_count: report
            .observed_lfs_object_count
            .map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
        expected_lfs_bytes: report.expected_lfs_bytes,
        observed_lfs_bytes: report.observed_lfs_bytes,
        expected_lfs_aggregate_sha256: report.expected_lfs_aggregate_sha256.clone(),
        observed_lfs_aggregate_sha256: report.observed_lfs_aggregate_sha256.clone(),
        network_disabled: report.network_disabled,
        live_mirror_accessed: report.live_mirror_accessed,
    }
}

const fn evidence_outcome(outcome: ReportOutcome) -> EvidenceOutcome {
    match outcome {
        ReportOutcome::Passed => EvidenceOutcome::Passed,
        ReportOutcome::Failed => EvidenceOutcome::Failed,
    }
}

const fn failure_code(failure: VerificationFailure) -> &'static str {
    match failure {
        VerificationFailure::HashMismatch => "hash_mismatch",
        VerificationFailure::ManifestInvalid => "manifest_invalid",
        VerificationFailure::BundleInvalid => "bundle_invalid",
        VerificationFailure::IsolationFailed => "isolation_failed",
        VerificationFailure::RefMismatch => "ref_mismatch",
        VerificationFailure::ReplicaUnavailable => "replica_unavailable",
        VerificationFailure::LfsInvalid => "lfs_invalid",
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn stages_json(stages: &[StageReport]) -> serde_json::Value {
    serde_json::Value::Array(
        stages
            .iter()
            .map(|stage| {
                serde_json::json!({
                    "stage": stage.stage,
                    "passed": stage.passed,
                    "duration_millis": duration_millis(stage.duration),
                })
            })
            .collect(),
    )
}
