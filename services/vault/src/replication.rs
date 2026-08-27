//! Bounded off-host replication admission, execution, and health projection.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::{ReplicaError, ReplicaStore};
use ratatoskr_vault_core::snapshot::BlobRef;
use ratatoskr_vault_persistence::Database;
use ratatoskr_vault_telemetry::metrics::{
    ReplicationDiagnosticFailure, ReplicationDiagnosticOutcome, record_replication_attempt,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// One immutable artifact/target transfer admitted to the replication worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationUnit {
    /// Database artifact identity used for the durable lease and placement.
    pub artifact_id: Uuid,
    /// Explicit configured target identity.
    pub replica_target_id: Uuid,
    /// Exact immutable local and remote byte identity.
    pub blob: BlobRef,
}

/// Finite worker resource and recovery bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicationWorkerSettings {
    /// Maximum transfers owned by this worker at once.
    pub max_concurrent: usize,
    /// Durable claim lease; cancellation leaves it to expire for safe recovery.
    pub lease: Duration,
}

/// Terminal result for one admitted unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationUnitOutcome {
    /// Remote bytes were re-downloaded, matched, and placement evidence committed.
    Verified,
    /// A bounded remote or persistence failure was recorded for later retry.
    Failed,
    /// Shutdown interrupted the unit while its finite durable lease remained recoverable.
    Cancelled,
}

/// One spawned transfer owned by the worker shutdown sequence.
#[derive(Debug)]
pub struct ReplicationRun {
    join: tokio::task::JoinHandle<ReplicationUnitOutcome>,
    accepting: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl ReplicationRun {
    /// Stops admission, grants the owned transfer a finite join window, then aborts and joins it.
    /// A forced stop deliberately leaves the durable claim running until its lease expires.
    pub async fn shutdown(mut self, deadline: Duration) -> ReplicationUnitOutcome {
        self.accepting.store(false, Ordering::Release);
        let _signalled = self.shutdown.send(true);
        match tokio::time::timeout(deadline, &mut self.join).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => ReplicationUnitOutcome::Cancelled,
            Err(_) => {
                self.join.abort();
                let _joined = self.join.await;
                ReplicationUnitOutcome::Cancelled
            }
        }
    }
}

/// Isolated off-host worker; local snapshot and verification paths own no worker permits.
#[derive(Debug, Clone)]
pub struct ReplicationWorker {
    database: Database,
    local_store: LocalBlobStore,
    replicas: Arc<HashMap<Uuid, ReplicaStore>>,
    settings: ReplicationWorkerSettings,
    permits: Arc<Semaphore>,
    accepting: Arc<AtomicBool>,
}

impl ReplicationWorker {
    /// Creates a bounded worker without admitting work.
    #[must_use]
    pub fn new(
        database: Database,
        local_store: LocalBlobStore,
        replicas: HashMap<Uuid, ReplicaStore>,
        settings: ReplicationWorkerSettings,
    ) -> Self {
        let permits = Arc::new(Semaphore::new(settings.max_concurrent.max(1)));
        Self {
            database,
            local_store,
            replicas: Arc::new(replicas),
            settings,
            permits,
            accepting: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Spawns one owned unit so shutdown can stop and join it explicitly.
    #[must_use]
    pub fn start_unit(&self, unit: ReplicationUnit) -> ReplicationRun {
        let worker = self.clone();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        ReplicationRun {
            join: tokio::spawn(async move { worker.run_unit_cancellable(unit, receiver).await }),
            accepting: Arc::clone(&self.accepting),
            shutdown,
        }
    }

    /// Claims, transfers, re-downloads, verifies, and terminally records one unit. Remote failure
    /// never consumes a permit used by local snapshot or verification work.
    pub async fn run_unit(&self, unit: ReplicationUnit) -> ReplicationUnitOutcome {
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        self.run_unit_cancellable(unit, receiver).await
    }

    async fn run_unit_cancellable(
        &self,
        unit: ReplicationUnit,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> ReplicationUnitOutcome {
        let started = std::time::Instant::now();
        let target_id = unit.replica_target_id.to_string();
        if !self.accepting.load(Ordering::Acquire) {
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Failed,
                Some(ReplicationDiagnosticFailure::Cancelled),
                started,
                0,
            );
            return ReplicationUnitOutcome::Cancelled;
        }
        let Ok(_permit) = self.permits.acquire().await else {
            return ReplicationUnitOutcome::Cancelled;
        };
        let Some(replica) = self.replicas.get(&unit.replica_target_id) else {
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Failed,
                Some(ReplicationDiagnosticFailure::RemoteAbsent),
                started,
                0,
            );
            return ReplicationUnitOutcome::Failed;
        };
        let lease_owner = Uuid::now_v7();
        let Ok(attempt_id) = self
            .database
            .claim_replication_attempt(
                unit.artifact_id,
                unit.replica_target_id,
                lease_owner,
                self.settings.lease,
            )
            .await
        else {
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Failed,
                Some(ReplicationDiagnosticFailure::Local),
                started,
                0,
            );
            return ReplicationUnitOutcome::Failed;
        };
        let Ok(source) = self.local_store.resolve(&unit.blob) else {
            let outcome = self
                .finish_failure(attempt_id, lease_owner, "local_unavailable")
                .await;
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Failed,
                Some(ReplicationDiagnosticFailure::Local),
                started,
                0,
            );
            return outcome;
        };
        match replica
            .upload_and_verify_cancellable(&unit.blob, &source, shutdown)
            .await
        {
            Ok(placement) => {
                self.finish_verified(
                    attempt_id,
                    lease_owner,
                    &placement.object_key,
                    &unit,
                    started,
                )
                .await
            }
            Err(ReplicaError::Cancelled) => {
                observe_attempt(
                    &target_id,
                    ReplicationDiagnosticOutcome::Failed,
                    Some(ReplicationDiagnosticFailure::Cancelled),
                    started,
                    0,
                );
                ReplicationUnitOutcome::Cancelled
            }
            Err(error) => {
                let outcome = self
                    .finish_failure(attempt_id, lease_owner, replica_failure_class(error))
                    .await;
                observe_attempt(
                    &target_id,
                    ReplicationDiagnosticOutcome::Failed,
                    Some(replica_diagnostic_failure(error)),
                    started,
                    0,
                );
                outcome
            }
        }
    }

    async fn finish_verified(
        &self,
        attempt_id: Uuid,
        lease_owner: Uuid,
        object_key: &str,
        unit: &ReplicationUnit,
        started: std::time::Instant,
    ) -> ReplicationUnitOutcome {
        let target_id = unit.replica_target_id.to_string();
        if self
            .database
            .record_verified_replica(attempt_id, lease_owner, object_key, &unit.blob)
            .await
            .is_ok()
        {
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Verified,
                None,
                started,
                unit.blob.size_bytes,
            );
            ReplicationUnitOutcome::Verified
        } else {
            observe_attempt(
                &target_id,
                ReplicationDiagnosticOutcome::Failed,
                Some(ReplicationDiagnosticFailure::Local),
                started,
                0,
            );
            ReplicationUnitOutcome::Failed
        }
    }

    async fn finish_failure(
        &self,
        attempt_id: Uuid,
        lease_owner: Uuid,
        failure_class: &'static str,
    ) -> ReplicationUnitOutcome {
        let _recorded = self
            .database
            .record_replication_failure(attempt_id, lease_owner, failure_class)
            .await;
        ReplicationUnitOutcome::Failed
    }
}

fn observe_attempt(
    target_id: &str,
    outcome: ReplicationDiagnosticOutcome,
    failure: Option<ReplicationDiagnosticFailure>,
    started: std::time::Instant,
    bytes: u64,
) {
    record_replication_attempt(target_id, outcome, failure, started.elapsed(), bytes);
}

const fn replica_diagnostic_failure(error: ReplicaError) -> ReplicationDiagnosticFailure {
    match error {
        ReplicaError::InvalidInput | ReplicaError::SourceIo | ReplicaError::DigestMismatch => {
            ReplicationDiagnosticFailure::Local
        }
        ReplicaError::Remote => ReplicationDiagnosticFailure::Remote,
        ReplicaError::NotFound => ReplicationDiagnosticFailure::RemoteAbsent,
        ReplicaError::Timeout => ReplicationDiagnosticFailure::Timeout,
        ReplicaError::RemoteChecksumMismatch => ReplicationDiagnosticFailure::Checksum,
        ReplicaError::RemoteTruncated | ReplicaError::RemoteOversized => {
            ReplicationDiagnosticFailure::Length
        }
        ReplicaError::SizeLimitExceeded => ReplicationDiagnosticFailure::SizeLimit,
        ReplicaError::Cancelled => ReplicationDiagnosticFailure::Cancelled,
    }
}

const fn replica_failure_class(error: ReplicaError) -> &'static str {
    match error {
        ReplicaError::InvalidInput => "invalid_input",
        ReplicaError::SourceIo => "local_io",
        ReplicaError::Remote => "remote",
        ReplicaError::NotFound => "remote_absent",
        ReplicaError::Timeout => "timeout",
        ReplicaError::DigestMismatch => "local_digest_mismatch",
        ReplicaError::RemoteChecksumMismatch => "remote_checksum_mismatch",
        ReplicaError::RemoteTruncated => "remote_truncated",
        ReplicaError::RemoteOversized => "remote_oversized",
        ReplicaError::SizeLimitExceeded => "size_limit",
        ReplicaError::Cancelled => "cancelled",
    }
}

/// One absent or stale artifact/target unit presented to the pure planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationCandidate {
    /// Snapshot owning the artifact.
    pub snapshot_id: Uuid,
    /// Artifact to replicate.
    pub artifact_id: Uuid,
    /// Target receiving the artifact.
    pub replica_target_id: Uuid,
    /// Immutable byte size used for admission.
    pub size_bytes: u64,
    /// Successful remote verification as Unix seconds; absent placements sort first.
    pub last_verified_at: Option<u64>,
}

/// Finite backlog policy for one target and one scheduler pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicationBacklogLimits {
    /// Maximum units admitted in one pass.
    pub max_items: usize,
    /// Maximum aggregate artifact bytes admitted in one pass.
    pub max_bytes: u64,
    /// Maximum concurrently executing admitted units.
    pub max_concurrent: usize,
}

/// Stable admitted window plus work left due for later passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationPlan {
    /// Units admitted for this pass.
    pub admitted: Vec<ReplicationCandidate>,
    /// Units explicitly preserved as due rather than silently discarded.
    pub deferred: Vec<ReplicationCandidate>,
    /// Finite execution concurrency carried into the worker.
    pub max_concurrent: usize,
}

/// Configured target participation in snapshot health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaRequirement {
    /// Stable replica target identity.
    pub replica_target_id: Uuid,
    /// Required targets must hold every artifact before health converges.
    pub required: bool,
    /// Disabled targets are not current convergence destinations.
    pub enabled: bool,
}

/// One artifact/target pair with current verified placement evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedPlacement {
    /// Snapshot artifact present remotely.
    pub artifact_id: Uuid,
    /// Replica target where it was verified.
    pub replica_target_id: Uuid,
}

/// Off-host component of snapshot health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaHealth {
    /// Every configured required target has every snapshot artifact.
    Complete,
    /// At least one required placement is absent.
    Degraded,
    /// No required target exists, which cannot satisfy an offsite-required policy.
    NoRequiredTarget,
}

/// Health projection that preserves local evidence separately from off-host convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHealth {
    /// Original local verification result; replica failures never rewrite it.
    pub local_verified: bool,
    /// Independently derived off-host state.
    pub replica_health: ReplicaHealth,
    /// Policy-level health across both independent components.
    pub overall_healthy: bool,
}

/// Projects local and off-host evidence independently; policy health requires both to pass.
#[must_use]
pub fn project_snapshot_health(
    local_verified: bool,
    artifact_ids: &[Uuid],
    targets: &[ReplicaRequirement],
    placements: &[VerifiedPlacement],
) -> SnapshotHealth {
    let required: Vec<_> = targets
        .iter()
        .filter(|target| target.enabled && target.required)
        .collect();
    let has_required = !required.is_empty();
    let all_complete = required.iter().all(|target| {
        artifact_ids.iter().all(|artifact_id| {
            placements.iter().any(|placement| {
                placement.artifact_id == *artifact_id
                    && placement.replica_target_id == target.replica_target_id
            })
        })
    });
    let replica_health = if !has_required {
        ReplicaHealth::NoRequiredTarget
    } else if all_complete {
        ReplicaHealth::Complete
    } else {
        ReplicaHealth::Degraded
    };
    SnapshotHealth {
        local_verified,
        replica_health,
        overall_healthy: local_verified && replica_health == ReplicaHealth::Complete,
    }
}

/// Orders absent/stale units deterministically and admits only what fits both finite ceilings.
#[must_use]
pub fn plan_replication_backlog(
    mut candidates: Vec<ReplicationCandidate>,
    limits: ReplicationBacklogLimits,
) -> ReplicationPlan {
    candidates.sort_by_key(|candidate| {
        (
            candidate.last_verified_at.is_some(),
            candidate.last_verified_at.unwrap_or(0),
            candidate.snapshot_id,
            candidate.artifact_id,
            candidate.replica_target_id,
        )
    });
    let mut admitted = Vec::new();
    let mut deferred = Vec::new();
    let mut admitted_bytes = 0_u64;
    for candidate in candidates {
        let next_bytes = admitted_bytes.checked_add(candidate.size_bytes);
        let fits = admitted.len() < limits.max_items
            && next_bytes.is_some_and(|bytes| bytes <= limits.max_bytes);
        if fits {
            admitted_bytes = next_bytes.unwrap_or(admitted_bytes);
            admitted.push(candidate);
        } else {
            deferred.push(candidate);
        }
    }
    ReplicationPlan {
        admitted,
        deferred,
        max_concurrent: limits.max_concurrent,
    }
}
