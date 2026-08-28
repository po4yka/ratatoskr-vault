//! Replica acquisition phase for isolated restore drills.

use std::path::Path;
use std::time::{Instant, SystemTime};

use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::{ReplicaError, ReplicaStore};
use uuid::Uuid;

use super::{
    DrillResult, RestoreDrill, RestoreDrillReport, RestoreSource, StageReport, VerificationFailure,
    VerificationReport, build_drill_report,
};

impl RestoreDrill {
    /// Acquires every drill artifact from one verified replica before invoking the isolated Git
    /// stages. Remote bytes are streamed into create-new files and republished into a drill-owned
    /// content-addressed store, so Git cannot observe an unverified partial download.
    #[must_use]
    pub async fn run_from_replica(
        &self,
        verification: &VerificationReport,
        replica_target_id: Uuid,
        replica: &ReplicaStore,
    ) -> RestoreDrillReport {
        let drill_id = Uuid::now_v7();
        let started_at = SystemTime::now();
        let started = Instant::now();
        let run_root = self
            .settings
            .scratch_root
            .join("runs")
            .join(drill_id.to_string());
        let result = self
            .execute_from_replica(&run_root, verification, replica)
            .await;
        let _ignored = std::fs::remove_dir_all(&run_root);
        build_drill_report(
            drill_id,
            verification,
            RestoreSource::Replica { replica_target_id },
            started_at,
            started.elapsed(),
            result,
        )
    }

    async fn execute_from_replica(
        &self,
        run_root: &Path,
        verification: &VerificationReport,
        replica: &ReplicaStore,
    ) -> DrillResult {
        let acquisition_started = Instant::now();
        let acquired_root = run_root.join("acquired");
        let staged_root = run_root.join("replica-store");
        if std::fs::create_dir_all(&acquired_root).is_err() {
            return acquisition_failure(acquisition_started, VerificationFailure::IsolationFailed);
        }
        let max_object_bytes = std::iter::once(&verification.manifest)
            .chain(verification.checked_artifacts.iter())
            .map(|reference| reference.size_bytes)
            .max()
            .unwrap_or(1)
            .max(1);
        let Ok(download_store) = LocalBlobStore::new(staged_root, max_object_bytes) else {
            return acquisition_failure(acquisition_started, VerificationFailure::IsolationFailed);
        };
        for (index, reference) in std::iter::once(&verification.manifest)
            .chain(verification.checked_artifacts.iter())
            .enumerate()
        {
            let destination = acquired_root.join(format!("{index}-{}", reference.sha256));
            if let Err(error) = replica.download_verified(reference, &destination).await {
                return acquisition_failure(acquisition_started, replica_failure(error));
            }
            if download_store
                .publish_file(reference, &destination)
                .is_err()
            {
                return acquisition_failure(acquisition_started, VerificationFailure::HashMismatch);
            }
        }
        let acquisition_stage = StageReport {
            stage: "replica_acquire",
            passed: true,
            duration: acquisition_started.elapsed(),
        };
        match self
            .execute(&run_root.join("repository"), verification, &download_store)
            .await
        {
            Ok((mut stages, refs, lfs)) => {
                stages.insert(0, acquisition_stage);
                Ok((stages, refs, lfs))
            }
            Err((mut stages, failure, refs, lfs)) => {
                stages.insert(0, acquisition_stage);
                Err((stages, failure, refs, lfs))
            }
        }
    }
}

fn acquisition_failure(started: Instant, failure: VerificationFailure) -> DrillResult {
    Err((
        vec![StageReport {
            stage: "replica_acquire",
            passed: false,
            duration: started.elapsed(),
        }],
        failure,
        Vec::new(),
        None,
    ))
}

const fn replica_failure(error: ReplicaError) -> VerificationFailure {
    match error {
        ReplicaError::DigestMismatch
        | ReplicaError::RemoteChecksumMismatch
        | ReplicaError::RemoteTruncated
        | ReplicaError::RemoteOversized
        | ReplicaError::SizeLimitExceeded => VerificationFailure::HashMismatch,
        ReplicaError::InvalidInput
        | ReplicaError::SourceIo
        | ReplicaError::Remote
        | ReplicaError::RemoteStillPresent
        | ReplicaError::NotFound
        | ReplicaError::Timeout
        | ReplicaError::Cancelled => VerificationFailure::ReplicaUnavailable,
    }
}
