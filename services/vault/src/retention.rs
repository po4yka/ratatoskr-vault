//! Bounded, durable local-first retention execution.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::{ReplicaPlacement, ReplicaStore};
use ratatoskr_vault_core::target_state::TargetStatus;
use ratatoskr_vault_persistence::{
    Database, DeletionExecutionArtifact, DeletionExecutionPlan, DeletionExecutionReplica,
    DeletionStageFailureClass, DeletionStageKind, StageClaimOutcome, StageClaimRequest,
};
use ratatoskr_vault_telemetry::metrics::{
    RetentionDiagnosticDecision, RetentionDiagnosticMode, RetentionDiagnosticReason,
    RetentionDiagnosticStage, RetentionDiagnosticStageOutcome, record_retention_decision,
    record_retention_stage,
};
use uuid::Uuid;

/// Finite execution bounds for retention work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionWorkerSettings {
    /// Durable lease for one exact physical effect.
    pub lease: Duration,
    /// Trusted root containing persisted relative bare-mirror paths.
    pub mirror_root: PathBuf,
}

/// Terminal result of one bounded deletion-plan pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPlanOutcome {
    /// Every required local and replica stage has terminal evidence.
    Completed,
    /// Grace or a newly observed pin prevented effects.
    Deferred,
    /// A storage or persistence operation failed and remains retryable.
    Failed,
}

/// Executor whose adapters are restricted to configured local and replica stores.
#[derive(Debug, Clone)]
pub struct RetentionWorker {
    database: Database,
    local_store: LocalBlobStore,
    replicas: HashMap<Uuid, ReplicaStore>,
    settings: RetentionWorkerSettings,
}

impl RetentionWorker {
    /// Builds a worker without executing or claiming deletion work.
    #[must_use]
    pub fn new(
        database: Database,
        local_store: LocalBlobStore,
        replicas: HashMap<Uuid, ReplicaStore>,
        settings: RetentionWorkerSettings,
    ) -> Self {
        Self {
            database,
            local_store,
            replicas,
            settings,
        }
    }

    /// Runs at most one durable plan to a terminal or retryable boundary.
    pub async fn run_plan(&self, plan_id: Uuid) -> RetentionPlanOutcome {
        self.run_plan_inner(plan_id)
            .await
            .unwrap_or(RetentionPlanOutcome::Failed)
    }

    async fn run_plan_inner(&self, plan_id: Uuid) -> Result<RetentionPlanOutcome, ()> {
        let plan = self
            .database
            .deletion_execution_plan(plan_id)
            .await
            .map_err(|_| ())?;
        if plan.status == "completed" {
            return Ok(RetentionPlanOutcome::Completed);
        }
        record_retention_decision(
            RetentionDiagnosticMode::Scheduled,
            RetentionDiagnosticDecision::Eligible,
            if plan.tombstone_id.is_some() {
                RetentionDiagnosticReason::EligibleInactiveTarget
            } else {
                RetentionDiagnosticReason::EligibleOrdinary
            },
        );
        if plan.tombstone_id.is_some() && plan.status == "planned" {
            self.database
                .set_target_status(plan.target_id, TargetStatus::Deleting)
                .await
                .map_err(|_| ())?;
        }
        if plan.status != "replica_deleting" {
            if !self.run_local_stages(&plan).await? {
                return Ok(RetentionPlanOutcome::Deferred);
            }
            if plan.tombstone_id.is_some() && !self.run_mirror_stage(&plan).await? {
                return Ok(RetentionPlanOutcome::Deferred);
            }
            self.database
                .advance_deletion_plan_to_replicas(plan_id)
                .await
                .map_err(|_| ())?;
        }
        if !self.run_replica_stages(&plan).await? {
            return Ok(RetentionPlanOutcome::Deferred);
        }
        self.database
            .complete_deletion_plan(plan_id)
            .await
            .map_err(|_| ())?;
        Ok(RetentionPlanOutcome::Completed)
    }

    async fn run_local_stages(&self, plan: &DeletionExecutionPlan) -> Result<bool, ()> {
        for artifact in &plan.artifacts {
            if !self.run_local_artifact(plan.plan_id, artifact).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run_local_artifact(
        &self,
        plan_id: Uuid,
        artifact: &DeletionExecutionArtifact,
    ) -> Result<bool, ()> {
        let started = std::time::Instant::now();
        let claim = self
            .database
            .claim_deletion_stage(&StageClaimRequest {
                plan_id,
                kind: DeletionStageKind::Local,
                stage_key: format!("local:{}", artifact.artifact_id),
                artifact_id: Some(artifact.artifact_id),
                replica_target_id: None,
                placement_id: None,
                lease_owner: Uuid::now_v7(),
                lease_seconds: self.settings.lease.as_secs(),
            })
            .await
            .map_err(|_| ())?;
        match claim_action(claim) {
            ClaimAction::Effect(attempt_id, claim_id) => {
                if self
                    .local_store
                    .delete_verified_blob(&artifact.blob)
                    .is_err()
                {
                    self.database
                        .record_deletion_stage_failure(
                            attempt_id,
                            claim_id,
                            DeletionStageFailureClass::LocalIo,
                        )
                        .await
                        .map_err(|_| ())?;
                    observe_stage_failure(RetentionDiagnosticStage::Local, "local_io", started);
                    return Err(());
                }
                self.database
                    .record_deletion_stage_success(attempt_id, claim_id)
                    .await
                    .map_err(|_| ())?;
                observe_stage_success(RetentionDiagnosticStage::Local, started);
                Ok(true)
            }
            ClaimAction::Suppressed => {
                observe_stage_suppressed(RetentionDiagnosticStage::Local, started);
                Ok(true)
            }
            ClaimAction::Deferred(reason) => {
                observe_refusal(reason);
                Ok(false)
            }
        }
    }

    async fn run_replica_stages(&self, plan: &DeletionExecutionPlan) -> Result<bool, ()> {
        for artifact in &plan.artifacts {
            for stored in &artifact.replicas {
                if !self.run_replica(plan.plan_id, artifact, stored).await? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn run_mirror_stage(&self, plan: &DeletionExecutionPlan) -> Result<bool, ()> {
        let started = std::time::Instant::now();
        let storage_path = plan.mirror_storage_path.as_deref().ok_or(())?;
        let claim = self
            .database
            .claim_deletion_stage(&StageClaimRequest {
                plan_id: plan.plan_id,
                kind: DeletionStageKind::MirrorLocal,
                stage_key: format!("mirror:{}", plan.target_id),
                artifact_id: None,
                replica_target_id: None,
                placement_id: None,
                lease_owner: Uuid::now_v7(),
                lease_seconds: self.settings.lease.as_secs(),
            })
            .await
            .map_err(|_| ())?;
        match claim_action(claim) {
            ClaimAction::Effect(attempt_id, claim_id) => {
                if delete_confined_mirror(&self.settings.mirror_root, storage_path).is_err() {
                    self.database
                        .record_deletion_stage_failure(
                            attempt_id,
                            claim_id,
                            DeletionStageFailureClass::LocalIo,
                        )
                        .await
                        .map_err(|_| ())?;
                    observe_stage_failure(
                        RetentionDiagnosticStage::MirrorLocal,
                        "mirror_io",
                        started,
                    );
                    return Err(());
                }
                self.database
                    .record_deletion_stage_success(attempt_id, claim_id)
                    .await
                    .map_err(|_| ())?;
                observe_stage_success(RetentionDiagnosticStage::MirrorLocal, started);
                Ok(true)
            }
            ClaimAction::Suppressed => {
                observe_stage_suppressed(RetentionDiagnosticStage::MirrorLocal, started);
                Ok(true)
            }
            ClaimAction::Deferred(reason) => {
                observe_refusal(reason);
                Ok(false)
            }
        }
    }

    async fn run_replica(
        &self,
        plan_id: Uuid,
        artifact: &DeletionExecutionArtifact,
        stored: &DeletionExecutionReplica,
    ) -> Result<bool, ()> {
        let started = std::time::Instant::now();
        let claim = self
            .database
            .claim_deletion_stage(&StageClaimRequest {
                plan_id,
                kind: DeletionStageKind::Replica,
                stage_key: format!("replica:{}", stored.placement_id),
                artifact_id: Some(artifact.artifact_id),
                replica_target_id: Some(stored.replica_target_id),
                placement_id: Some(stored.placement_id),
                lease_owner: Uuid::now_v7(),
                lease_seconds: self.settings.lease.as_secs(),
            })
            .await
            .map_err(|_| ())?;
        match claim_action(claim) {
            ClaimAction::Effect(attempt_id, claim_id) => {
                let Some(replica) = self.replicas.get(&stored.replica_target_id) else {
                    self.database
                        .record_deletion_stage_failure(
                            attempt_id,
                            claim_id,
                            DeletionStageFailureClass::ReplicaUnavailable,
                        )
                        .await
                        .map_err(|_| ())?;
                    observe_stage_failure(
                        RetentionDiagnosticStage::Replica,
                        "replica_unavailable",
                        started,
                    );
                    return Err(());
                };
                let placement = ReplicaPlacement {
                    target: replica.target().to_owned(),
                    object_key: stored.object_key.clone(),
                    size_bytes: artifact.blob.size_bytes,
                    sha256: artifact.blob.sha256.clone(),
                };
                if replica
                    .delete_verified(&artifact.blob, &placement)
                    .await
                    .is_err()
                {
                    self.database
                        .record_deletion_stage_failure(
                            attempt_id,
                            claim_id,
                            DeletionStageFailureClass::RemoteVerification,
                        )
                        .await
                        .map_err(|_| ())?;
                    observe_stage_failure(RetentionDiagnosticStage::Replica, "remote", started);
                    return Err(());
                }
                self.database
                    .record_deletion_stage_success(attempt_id, claim_id)
                    .await
                    .map_err(|_| ())?;
                observe_stage_success(RetentionDiagnosticStage::Replica, started);
                Ok(true)
            }
            ClaimAction::Suppressed => {
                observe_stage_suppressed(RetentionDiagnosticStage::Replica, started);
                Ok(true)
            }
            ClaimAction::Deferred(reason) => {
                observe_refusal(reason);
                Ok(false)
            }
        }
    }
}

fn delete_confined_mirror(root: &Path, storage_path: &str) -> Result<(), std::io::Error> {
    let relative = Path::new(storage_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(std::io::Error::other("mirror path is not confined"));
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let target = canonical_root.join(relative);
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::other(
            "mirror target is not a regular directory",
        ));
    }
    let canonical_target = std::fs::canonicalize(&target)?;
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        return Err(std::io::Error::other("mirror target escapes its root"));
    }
    validate_mirror_tree(&canonical_target)?;
    std::fs::remove_dir_all(&canonical_target)?;
    match std::fs::symlink_metadata(&canonical_target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(std::io::Error::other("mirror remains after deletion")),
    }
}

fn validate_mirror_tree(directory: &Path) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            validate_mirror_tree(&entry.path())?;
        } else if !metadata.is_file() {
            return Err(std::io::Error::other(
                "mirror contains a symlink or non-regular entry",
            ));
        }
    }
    Ok(())
}

enum ClaimAction {
    Effect(Uuid, Uuid),
    Suppressed,
    Deferred(RetentionDiagnosticReason),
}

const fn claim_action(outcome: StageClaimOutcome) -> ClaimAction {
    match outcome {
        StageClaimOutcome::Claimed {
            attempt_id,
            claim_id,
        } => ClaimAction::Effect(attempt_id, claim_id),
        StageClaimOutcome::SharedReferenceRetained { .. } => ClaimAction::Suppressed,
        StageClaimOutcome::GraceActive => {
            ClaimAction::Deferred(RetentionDiagnosticReason::GraceActive)
        }
        StageClaimOutcome::ProtectedPinned => {
            ClaimAction::Deferred(RetentionDiagnosticReason::ProtectedPinned)
        }
    }
}

fn observe_stage_failure(
    stage: RetentionDiagnosticStage,
    failure: &'static str,
    started: std::time::Instant,
) {
    record_retention_stage(
        stage,
        RetentionDiagnosticStageOutcome::Failed,
        Some(failure),
        started.elapsed(),
    );
}

fn observe_stage_success(stage: RetentionDiagnosticStage, started: std::time::Instant) {
    record_retention_stage(
        stage,
        RetentionDiagnosticStageOutcome::Succeeded,
        None,
        started.elapsed(),
    );
}

fn observe_stage_suppressed(stage: RetentionDiagnosticStage, started: std::time::Instant) {
    record_retention_stage(
        stage,
        RetentionDiagnosticStageOutcome::SharedReferenceRetained,
        None,
        started.elapsed(),
    );
}

fn observe_refusal(reason: RetentionDiagnosticReason) {
    record_retention_decision(
        RetentionDiagnosticMode::Scheduled,
        RetentionDiagnosticDecision::Refused,
        reason,
    );
}

#[cfg(test)]
mod tests {
    use super::delete_confined_mirror;

    #[test]
    fn mirror_deletion_refuses_symlinks_without_touching_their_target() {
        let fixture =
            std::env::temp_dir().join(format!("vault-mirror-delete-{}", uuid::Uuid::now_v7()));
        let root = fixture.join("owned");
        let mirror = root.join("mirrors/ab/00000000-0000-0000-0000-000000000001.git");
        let outside = fixture.join("outside");
        std::fs::create_dir_all(&mirror).expect("mirror fixture");
        std::fs::create_dir_all(&outside).expect("outside fixture");
        std::fs::write(outside.join("evidence"), b"must remain").expect("outside evidence");
        std::os::unix::fs::symlink(&outside, mirror.join("unsafe-link")).expect("symlink fixture");

        let outcome =
            delete_confined_mirror(&root, "mirrors/ab/00000000-0000-0000-0000-000000000001.git");

        assert!(outcome.is_err());
        assert!(mirror.exists());
        assert!(outside.join("evidence").exists());
        std::fs::remove_dir_all(fixture).expect("fixture cleanup");
    }
}
