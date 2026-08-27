//! Immutable manifest-chain and artifact-byte verification.

use ratatoskr_vault_blobstore::BlobStoreError;
use ratatoskr_vault_core::snapshot::{ManifestError, verify_manifest_chain};

use super::{
    ArtifactVerifier, BlobRef, LocalBlobStore, ManifestVerificationKey, ReportOutcome,
    SnapshotManifest, StageReport, SystemTime, Uuid, VerificationFailure, VerificationReport,
    canonical_ref_digest,
};

impl ArtifactVerifier {
    /// Creates a verifier with an explicit trust set and finite chain bound.
    #[must_use]
    pub const fn new(
        store: LocalBlobStore,
        trusted_keys: Vec<ManifestVerificationKey>,
        max_chain_depth: usize,
    ) -> Self {
        Self {
            store,
            trusted_keys,
            max_chain_depth,
        }
    }

    /// Produces a terminal report for the supplied immutable manifest.
    #[must_use]
    pub fn verify(&self, snapshot_id: Uuid, manifest: BlobRef) -> VerificationReport {
        let started_at = SystemTime::now();
        let started = std::time::Instant::now();
        let mut stages = Vec::new();
        if self
            .record_blob_check(&manifest, "manifest_hash", &mut stages)
            .is_err()
        {
            return terminal_verification_report(
                snapshot_id,
                manifest,
                started_at,
                started,
                stages,
                Vec::new(),
                None,
                VerificationFailure::HashMismatch,
            );
        }
        let Ok(evidence) = self.load_manifest(&manifest) else {
            return terminal_verification_report(
                snapshot_id,
                manifest,
                started_at,
                started,
                stages,
                Vec::new(),
                None,
                VerificationFailure::ManifestInvalid,
            );
        };
        let chain_started = std::time::Instant::now();
        let chain = verify_manifest_chain(
            &manifest,
            &self.trusted_keys,
            self.max_chain_depth,
            |reference| self.load_manifest(reference),
        );
        stages.push(StageReport {
            stage: "manifest_chain",
            passed: chain.is_ok(),
            duration: chain_started.elapsed(),
        });
        if chain.is_err() {
            return terminal_verification_report(
                snapshot_id,
                manifest,
                started_at,
                started,
                stages,
                Vec::new(),
                Some(&evidence),
                VerificationFailure::ManifestInvalid,
            );
        }
        let mut checked_artifacts = Vec::new();
        if let Err(failure) =
            self.record_artifact_checks(&evidence, &mut checked_artifacts, &mut stages)
        {
            return terminal_verification_report(
                snapshot_id,
                manifest,
                started_at,
                started,
                stages,
                checked_artifacts,
                Some(&evidence),
                failure,
            );
        }
        VerificationReport {
            verification_id: Uuid::now_v7(),
            snapshot_id,
            manifest,
            started_at,
            finished_at: SystemTime::now(),
            duration: started.elapsed(),
            outcome: ReportOutcome::Passed,
            failure: None,
            stages,
            checked_artifacts,
            expected_ref_count: evidence.refs.len(),
            expected_ref_set_sha256: evidence.ref_set_sha256,
            expected_lfs_object_count: evidence.lfs.as_ref().map(|lfs| lfs.objects.len()),
            expected_lfs_bytes: evidence.lfs.as_ref().map(|lfs| lfs.total_bytes),
            expected_lfs_aggregate_sha256: evidence
                .lfs
                .as_ref()
                .map(|lfs| lfs.aggregate_sha256.clone()),
        }
    }

    fn record_artifact_checks(
        &self,
        evidence: &SnapshotManifest,
        checked: &mut Vec<BlobRef>,
        stages: &mut Vec<StageReport>,
    ) -> Result<(), VerificationFailure> {
        for bundle in &evidence.bundles {
            checked.push(bundle.clone());
            self.record_blob_check(bundle, "bundle_hash", stages)
                .map_err(|_| VerificationFailure::HashMismatch)?;
        }
        if let Some(lfs) = &evidence.lfs {
            for object in &lfs.objects {
                checked.push(object.blob.clone());
                self.record_blob_check(&object.blob, "lfs_object_hash", stages)
                    .map_err(|_| VerificationFailure::LfsInvalid)?;
            }
        }
        Ok(())
    }

    fn record_blob_check(
        &self,
        reference: &BlobRef,
        stage: &'static str,
        stages: &mut Vec<StageReport>,
    ) -> Result<(), BlobStoreError> {
        let started = std::time::Instant::now();
        let result = self.store.verify(reference);
        stages.push(StageReport {
            stage,
            passed: result.is_ok(),
            duration: started.elapsed(),
        });
        result
    }

    fn load_manifest(&self, reference: &BlobRef) -> Result<SnapshotManifest, ManifestError> {
        self.store
            .verify(reference)
            .map_err(|_| ManifestError::MissingManifest)?;
        let path = self
            .store
            .resolve(reference)
            .map_err(|_| ManifestError::MissingManifest)?;
        let bytes = std::fs::read(path).map_err(|_| ManifestError::MissingManifest)?;
        serde_json::from_slice(&bytes).map_err(|_| ManifestError::Serialization)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "terminal report construction carries every immutable evidence field explicitly"
)]
fn terminal_verification_report(
    snapshot_id: Uuid,
    manifest: BlobRef,
    started_at: SystemTime,
    started: std::time::Instant,
    stages: Vec<StageReport>,
    checked_artifacts: Vec<BlobRef>,
    evidence: Option<&SnapshotManifest>,
    failure: VerificationFailure,
) -> VerificationReport {
    VerificationReport {
        verification_id: Uuid::now_v7(),
        snapshot_id,
        manifest,
        started_at,
        finished_at: SystemTime::now(),
        duration: started.elapsed(),
        outcome: ReportOutcome::Failed,
        failure: Some(failure),
        stages,
        checked_artifacts,
        expected_ref_count: evidence.map_or(0, |value| value.refs.len()),
        expected_ref_set_sha256: evidence.map_or_else(
            || canonical_ref_digest(&[]),
            |value| value.ref_set_sha256.clone(),
        ),
        expected_lfs_object_count: evidence
            .and_then(|value| value.lfs.as_ref())
            .map(|lfs| lfs.objects.len()),
        expected_lfs_bytes: evidence
            .and_then(|value| value.lfs.as_ref())
            .map(|lfs| lfs.total_bytes),
        expected_lfs_aggregate_sha256: evidence
            .and_then(|value| value.lfs.as_ref())
            .map(|lfs| lfs.aggregate_sha256.clone()),
    }
}
