//! Scheduled verification admission and isolated restore-drill execution.

mod durable;
mod replica;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ratatoskr_vault_blobstore::{BlobStoreError, LocalBlobStore};
use ratatoskr_vault_core::snapshot::{
    BlobRef, ManifestError, ManifestVerificationKey, RefEvidence, SnapshotManifest,
    canonical_ref_digest, verify_manifest_chain,
};
use ratatoskr_vault_gitrunner::{
    ConfinedPath, GitOperation, GitRunner, GitRunnerError, RunConfig, RunOutcome, Subcommand,
};
use uuid::Uuid;

pub use self::durable::{DurableVerification, VerificationExecution, plan_due_from_database};

/// Terminal state of a verification or drill report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOutcome {
    /// Every required stage passed.
    Passed,
    /// At least one required stage failed.
    Failed,
}

/// Stable failure classes safe for persistence, metrics, and alert routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFailure {
    /// Stored bytes differ from their immutable reference.
    HashMismatch,
    /// A manifest is absent, malformed, untrusted, or has a broken chain.
    ManifestInvalid,
    /// Git rejected the stored bundle.
    BundleInvalid,
    /// Restore confinement was refused or violated.
    IsolationFailed,
    /// Restored ref names or object ids differ from the manifest.
    RefMismatch,
    /// The selected replica could not supply verified bytes within its finite bounds.
    ReplicaUnavailable,
}
/// Operator policy for choosing drill bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSourcePolicy {
    /// Use only the local immutable store.
    Local,
    /// Prefer an eligible verified replica, otherwise use local bytes.
    ReplicaPreferred,
    /// Require eligible replica bytes and never fall back locally.
    ReplicaRequired,
}
/// One complete placement set considered for a replica-backed drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaRestoreCandidate {
    /// Stable replica identity.
    pub replica_target_id: Uuid,
    /// Every manifest-required artifact currently has verified placement evidence.
    pub complete: bool,
    /// Oldest successful verification across the placement set, as Unix seconds.
    pub verified_at: u64,
}
/// Actual source selected for a drill and persisted in its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSource {
    /// Vault's local immutable `BlobStore`.
    Local,
    /// One named replica target.
    Replica {
        /// Stable credential-free target identity.
        replica_target_id: Uuid,
    },
}

/// No source satisfies the requested drill policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("requested restore source is unavailable")]
pub struct RestoreSourceUnavailable;

/// Selects a source deterministically from complete placement evidence and a freshness cutoff.
///
/// # Errors
/// Returns [`RestoreSourceUnavailable`] when no eligible source satisfies `policy`.
pub fn select_restore_source(
    policy: RestoreSourcePolicy,
    local_available: bool,
    freshness_cutoff: u64,
    replicas: &[ReplicaRestoreCandidate],
) -> Result<RestoreSource, RestoreSourceUnavailable> {
    let replica = replicas
        .iter()
        .filter(|candidate| candidate.complete && candidate.verified_at >= freshness_cutoff)
        .min_by_key(|candidate| {
            (
                std::cmp::Reverse(candidate.verified_at),
                candidate.replica_target_id,
            )
        })
        .map(|candidate| RestoreSource::Replica {
            replica_target_id: candidate.replica_target_id,
        });
    match policy {
        RestoreSourcePolicy::Local if local_available => Ok(RestoreSource::Local),
        RestoreSourcePolicy::Local => Err(RestoreSourceUnavailable),
        RestoreSourcePolicy::ReplicaPreferred => replica
            .or_else(|| local_available.then_some(RestoreSource::Local))
            .ok_or(RestoreSourceUnavailable),
        RestoreSourcePolicy::ReplicaRequired => replica.ok_or(RestoreSourceUnavailable),
    }
}

/// One bounded stage observation inside a terminal report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReport {
    /// Stable bounded stage name.
    pub stage: &'static str,
    /// Whether this stage passed.
    pub passed: bool,
    /// Time spent in this stage.
    pub duration: Duration,
}

/// Complete immutable evidence from re-verifying one stored snapshot.
#[derive(Debug, Clone)]
pub struct VerificationReport {
    /// Stable verification attempt identity.
    pub verification_id: Uuid,
    /// Snapshot whose artifacts were checked.
    pub snapshot_id: Uuid,
    /// Exact manifest bytes used as authority.
    pub manifest: BlobRef,
    /// Start wall-clock observation.
    pub started_at: SystemTime,
    /// Completion wall-clock observation.
    pub finished_at: SystemTime,
    /// Total elapsed time.
    pub duration: Duration,
    /// Terminal outcome.
    pub outcome: ReportOutcome,
    /// Typed failure when unsuccessful.
    pub failure: Option<VerificationFailure>,
    /// Every stage reached, including the terminal failed stage.
    pub stages: Vec<StageReport>,
    /// Immutable artifact references checked.
    pub checked_artifacts: Vec<BlobRef>,
    /// Manifest ref count.
    pub expected_ref_count: usize,
    /// Canonical manifest ref-set digest.
    pub expected_ref_set_sha256: String,
}

/// Trusted filesystem and process bounds for restore drills.
#[derive(Debug, Clone)]
pub struct RestoreDrillSettings {
    /// Vault-owned scratch root; every drill gets one UUID-derived child.
    pub scratch_root: PathBuf,
    /// Live mirror root that drill operands are forbidden to enter.
    pub live_mirror_root: PathBuf,
    /// Trusted system Git binary.
    pub git_binary: PathBuf,
    /// Finite process deadline shared by typed Git stages.
    pub deadline: Duration,
}

/// Restore roots or deadline do not satisfy confinement requirements.
#[derive(Debug)]
pub struct InvalidRestoreDrillSettings;

impl core::fmt::Display for InvalidRestoreDrillSettings {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("restore drill settings are not confined and finite")
    }
}

impl std::error::Error for InvalidRestoreDrillSettings {}

/// Complete immutable evidence from reconstructing one stored snapshot.
#[derive(Debug, Clone)]
pub struct RestoreDrillReport {
    /// Stable drill attempt identity.
    pub drill_id: Uuid,
    /// Snapshot restored from its artifact.
    pub snapshot_id: Uuid,
    /// Exact manifest used for ref authority.
    pub manifest: BlobRef,
    /// Actual byte source used by the drill.
    pub source: RestoreSource,
    /// Start wall-clock observation.
    pub started_at: SystemTime,
    /// Completion wall-clock observation.
    pub finished_at: SystemTime,
    /// Total elapsed time.
    pub duration: Duration,
    /// Terminal outcome.
    pub outcome: ReportOutcome,
    /// Typed failure when unsuccessful.
    pub failure: Option<VerificationFailure>,
    /// Every Git and comparison stage reached.
    pub stages: Vec<StageReport>,
    /// Expected manifest ref count.
    pub expected_ref_count: usize,
    /// Restored ref count.
    pub observed_ref_count: usize,
    /// Expected canonical ref digest.
    pub expected_ref_set_sha256: String,
    /// Restored canonical ref digest.
    pub observed_ref_set_sha256: String,
    /// True because drill operations accept local paths only and set file-only protocol policy.
    pub network_disabled: bool,
    /// Must remain false: the live mirror root is a denied operand root.
    pub live_mirror_accessed: bool,
}

/// Reconstructs a repository from a stored bundle under finite confinement.
#[derive(Debug, Clone)]
pub struct RestoreDrill {
    settings: RestoreDrillSettings,
    store: LocalBlobStore,
}

impl RestoreDrill {
    /// Creates a drill runner after validating its roots and finite deadline.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRestoreDrillSettings`] for relative/overlapping roots or a zero deadline.
    pub fn new(
        settings: RestoreDrillSettings,
        store: LocalBlobStore,
    ) -> Result<Self, InvalidRestoreDrillSettings> {
        if !settings.scratch_root.is_absolute()
            || !settings.live_mirror_root.is_absolute()
            || settings.deadline.is_zero()
        {
            return Err(InvalidRestoreDrillSettings);
        }
        std::fs::create_dir_all(&settings.scratch_root).map_err(|_| InvalidRestoreDrillSettings)?;
        let scratch = settings
            .scratch_root
            .canonicalize()
            .map_err(|_| InvalidRestoreDrillSettings)?;
        let live = settings
            .live_mirror_root
            .canonicalize()
            .map_err(|_| InvalidRestoreDrillSettings)?;
        let store_root = store
            .root()
            .canonicalize()
            .map_err(|_| InvalidRestoreDrillSettings)?;
        if paths_overlap(&scratch, &live)
            || paths_overlap(&scratch, &store_root)
            || paths_overlap(&store_root, &live)
        {
            return Err(InvalidRestoreDrillSettings);
        }
        Ok(Self { settings, store })
    }

    /// Executes one terminal restore drill from the verified stored manifest.
    #[must_use]
    pub async fn run(&self, verification: &VerificationReport) -> RestoreDrillReport {
        let drill_id = Uuid::now_v7();
        let started_at = SystemTime::now();
        let started = std::time::Instant::now();
        let run_root = self
            .settings
            .scratch_root
            .join("runs")
            .join(drill_id.to_string());
        let result = self
            .execute(&run_root.join("repository"), verification, &self.store)
            .await;
        let _ignored = std::fs::remove_dir_all(&run_root);
        build_drill_report(
            drill_id,
            verification,
            RestoreSource::Local,
            started_at,
            started.elapsed(),
            result,
        )
    }

    async fn execute(
        &self,
        run_root: &std::path::Path,
        verification: &VerificationReport,
        store: &LocalBlobStore,
    ) -> DrillResult {
        let mut stages = Vec::new();
        if verification.outcome != ReportOutcome::Passed {
            return Err((stages, VerificationFailure::BundleInvalid, Vec::new()));
        }
        let evidence = Self::load_manifest(store, &verification.manifest)
            .map_err(|failure| (stages.clone(), failure, Vec::new()))?;
        let bundle = evidence.bundles.first().ok_or_else(|| {
            (
                stages.clone(),
                VerificationFailure::BundleInvalid,
                Vec::new(),
            )
        })?;
        store.verify(bundle).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::HashMismatch,
                Vec::new(),
            )
        })?;
        std::fs::create_dir_all(run_root).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::IsolationFailed,
                Vec::new(),
            )
        })?;
        let relative_bundle = store
            .resolve(bundle)
            .and_then(|path| {
                path.strip_prefix(store.root())
                    .map(std::path::Path::to_path_buf)
                    .map_err(|_| BlobStoreError::InvalidInput)
            })
            .map_err(|_| {
                (
                    stages.clone(),
                    VerificationFailure::IsolationFailed,
                    Vec::new(),
                )
            })?;
        let bundle_path = ConfinedPath::new(store.root(), &relative_bundle).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::IsolationFailed,
                Vec::new(),
            )
        })?;
        let runner = self.runner(run_root);
        run_git_stage(&runner, GitOperation::init_bare(), "init", &mut stages).await?;
        run_git_stage(
            &runner,
            GitOperation::bundle_verify(&bundle_path),
            "bundle_verify",
            &mut stages,
        )
        .await?;
        run_git_stage(
            &runner,
            GitOperation::fetch_bundle(&bundle_path),
            "fetch",
            &mut stages,
        )
        .await?;
        run_git_stage(&runner, GitOperation::fsck_full(), "fsck", &mut stages).await?;
        let refs =
            run_git_stage(&runner, GitOperation::show_ref(), "show_ref", &mut stages).await?;
        let observed = parse_ref_output(&refs.stdout)
            .map_err(|failure| (stages.clone(), failure, Vec::new()))?;
        let compare_started = std::time::Instant::now();
        let matches = observed == evidence.refs;
        stages.push(StageReport {
            stage: "ref_compare",
            passed: matches,
            duration: compare_started.elapsed(),
        });
        if matches {
            Ok((stages, observed))
        } else {
            Err((stages, VerificationFailure::RefMismatch, observed))
        }
    }

    fn load_manifest(
        store: &LocalBlobStore,
        reference: &BlobRef,
    ) -> Result<SnapshotManifest, VerificationFailure> {
        store
            .verify(reference)
            .map_err(|_| VerificationFailure::HashMismatch)?;
        let path = store
            .resolve(reference)
            .map_err(|_| VerificationFailure::ManifestInvalid)?;
        let bytes = std::fs::read(path).map_err(|_| VerificationFailure::ManifestInvalid)?;
        serde_json::from_slice(&bytes).map_err(|_| VerificationFailure::ManifestInvalid)
    }

    fn runner(&self, run_root: &std::path::Path) -> GitRunner {
        GitRunner::new(RunConfig {
            git_binary: self.settings.git_binary.clone(),
            allowed: vec![
                Subcommand::Init,
                Subcommand::Bundle,
                Subcommand::Fetch,
                Subcommand::Fsck,
                Subcommand::ShowRef,
            ],
            working_directory: run_root.to_path_buf(),
            run_home: run_root.join("runner-home"),
            deadline: self.settings.deadline,
            stdout_cap_bytes: 64 * 1024,
            stderr_cap_bytes: 64 * 1024,
            credential_helper: PathBuf::from("/usr/bin/false"),
        })
        .with_denied_roots(vec![self.settings.live_mirror_root.clone()])
    }
}

type DrillResult = Result<
    (Vec<StageReport>, Vec<RefEvidence>),
    (Vec<StageReport>, VerificationFailure, Vec<RefEvidence>),
>;

fn build_drill_report(
    drill_id: Uuid,
    verification: &VerificationReport,
    source: RestoreSource,
    started_at: SystemTime,
    duration: Duration,
    result: DrillResult,
) -> RestoreDrillReport {
    let (stages, observed_refs, failure) = match result {
        Ok((stages, refs)) => (stages, refs, None),
        Err((stages, failure, refs)) => (stages, refs, Some(failure)),
    };
    let observed_ref_set_sha256 = canonical_ref_digest(&observed_refs);
    RestoreDrillReport {
        drill_id,
        snapshot_id: verification.snapshot_id,
        manifest: verification.manifest.clone(),
        source,
        started_at,
        finished_at: SystemTime::now(),
        duration,
        outcome: if failure.is_none() {
            ReportOutcome::Passed
        } else {
            ReportOutcome::Failed
        },
        failure,
        stages,
        expected_ref_count: verification.expected_ref_count,
        observed_ref_count: observed_refs.len(),
        expected_ref_set_sha256: verification.expected_ref_set_sha256.clone(),
        observed_ref_set_sha256,
        network_disabled: true,
        live_mirror_accessed: false,
    }
}

fn paths_overlap(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

async fn run_git_stage(
    runner: &GitRunner,
    operation: GitOperation,
    stage: &'static str,
    stages: &mut Vec<StageReport>,
) -> Result<RunOutcome, (Vec<StageReport>, VerificationFailure, Vec<RefEvidence>)> {
    let started = std::time::Instant::now();
    let result = runner.run(&operation).await;
    let passed = result.as_ref().is_ok_and(|outcome| outcome.exit_code == 0);
    stages.push(StageReport {
        stage,
        passed,
        duration: started.elapsed(),
    });
    match result {
        Ok(outcome) if outcome.exit_code == 0 => Ok(outcome),
        Err(GitRunnerError::PathOutsideConfinement { .. }) => Err((
            stages.clone(),
            VerificationFailure::IsolationFailed,
            Vec::new(),
        )),
        Ok(_) | Err(_) => Err((
            stages.clone(),
            VerificationFailure::BundleInvalid,
            Vec::new(),
        )),
    }
}

fn parse_ref_output(bytes: &[u8]) -> Result<Vec<RefEvidence>, VerificationFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| VerificationFailure::BundleInvalid)?;
    let mut refs = Vec::new();
    for line in text.lines() {
        let (oid, name) = line
            .split_once(' ')
            .ok_or(VerificationFailure::BundleInvalid)?;
        refs.push(RefEvidence {
            name: name.to_owned(),
            oid: oid.to_owned(),
        });
    }
    refs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(refs)
}

/// Re-verifies immutable snapshot bytes without mutating them.
#[derive(Debug, Clone)]
pub struct ArtifactVerifier {
    store: LocalBlobStore,
    trusted_keys: Vec<ManifestVerificationKey>,
    max_chain_depth: usize,
}

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
        for bundle in &evidence.bundles {
            checked_artifacts.push(bundle.clone());
            if self
                .record_blob_check(bundle, "bundle_hash", &mut stages)
                .is_err()
            {
                return terminal_verification_report(
                    snapshot_id,
                    manifest,
                    started_at,
                    started,
                    stages,
                    checked_artifacts,
                    Some(&evidence),
                    VerificationFailure::HashMismatch,
                );
            }
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
        }
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
    }
}

/// Finite scheduling and execution budgets for verification work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationPolicy {
    /// Minimum seconds between successful artifact verifications.
    pub verification_frequency_seconds: u64,
    /// Minimum seconds between successful restore drills.
    pub drill_frequency_seconds: u64,
    /// Maximum candidates admitted by one schedule pass.
    pub sample_size: usize,
    /// Maximum declared bundle bytes admitted by one pass.
    pub scratch_byte_budget: u64,
    /// Maximum concurrent executions admitted by one pass.
    pub max_concurrent: usize,
    /// Finite wall-clock seconds for one drill.
    pub per_drill_timeout_seconds: u64,
}

/// A scheduling policy has a zero or contradictory finite bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidVerificationPolicy;

impl core::fmt::Display for InvalidVerificationPolicy {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("verification policy requires positive finite bounds")
    }
}

impl std::error::Error for InvalidVerificationPolicy {}

/// One immutable snapshot considered by the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleCandidate {
    /// Snapshot identity.
    pub snapshot_id: Uuid,
    /// Declared stored bundle size.
    pub bundle_size_bytes: u64,
    /// Unix timestamp of latest successful verification, absent when never verified.
    pub last_verified_at: Option<u64>,
    /// Unix timestamp of latest successful restore drill, absent when never drilled.
    pub last_drilled_at: Option<u64>,
}

/// Why a due candidate was not admitted in the current pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralReason {
    /// The configured sample or concurrency ceiling was reached.
    Capacity,
    /// The candidate would exceed the finite scratch-byte budget.
    ScratchBudget,
}

/// A due candidate explicitly deferred for a later schedule pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredCandidate {
    /// Snapshot identity.
    pub snapshot_id: Uuid,
    /// Admission limit that refused it this pass.
    pub reason: DeferralReason,
}

/// Deterministic output of one schedule pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulePlan {
    /// Due snapshots admitted in execution order.
    pub selected: Vec<Uuid>,
    /// Selected snapshots whose independent drill interval is due after verification passes.
    pub drill_selected: Vec<Uuid>,
    /// Due snapshots left for a later pass with truthful reasons.
    pub deferred: Vec<DeferredCandidate>,
}

/// Selects due work without opening any artifact.
///
/// # Errors
///
/// Returns [`InvalidVerificationPolicy`] when any admission bound is zero.
pub fn plan_due_snapshots(
    now_unix_seconds: u64,
    policy: VerificationPolicy,
    candidates: Vec<ScheduleCandidate>,
) -> Result<SchedulePlan, InvalidVerificationPolicy> {
    policy.validate()?;
    let mut due: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.is_due(now_unix_seconds, policy))
        .collect();
    due.sort_by_key(|candidate| {
        (
            candidate
                .last_verified_at
                .into_iter()
                .chain(candidate.last_drilled_at)
                .min()
                .unwrap_or(0),
            candidate.snapshot_id,
        )
    });

    let capacity = policy.sample_size.min(policy.max_concurrent);
    let mut selected = Vec::new();
    let mut drill_selected = Vec::new();
    let mut deferred = Vec::new();
    let mut admitted_bytes = 0_u64;
    for candidate in due {
        if selected.len() >= capacity {
            deferred.push(DeferredCandidate {
                snapshot_id: candidate.snapshot_id,
                reason: DeferralReason::Capacity,
            });
            continue;
        }
        let Some(next_bytes) = admitted_bytes.checked_add(candidate.bundle_size_bytes) else {
            deferred.push(DeferredCandidate {
                snapshot_id: candidate.snapshot_id,
                reason: DeferralReason::ScratchBudget,
            });
            continue;
        };
        if next_bytes > policy.scratch_byte_budget {
            deferred.push(DeferredCandidate {
                snapshot_id: candidate.snapshot_id,
                reason: DeferralReason::ScratchBudget,
            });
            continue;
        }
        admitted_bytes = next_bytes;
        if candidate.drill_is_due(now_unix_seconds, policy) {
            drill_selected.push(candidate.snapshot_id);
        }
        selected.push(candidate.snapshot_id);
    }
    Ok(SchedulePlan {
        selected,
        drill_selected,
        deferred,
    })
}

impl VerificationPolicy {
    fn validate(self) -> Result<(), InvalidVerificationPolicy> {
        if self.verification_frequency_seconds == 0
            || self.drill_frequency_seconds == 0
            || self.sample_size == 0
            || self.scratch_byte_budget == 0
            || self.max_concurrent == 0
            || self.per_drill_timeout_seconds == 0
        {
            Err(InvalidVerificationPolicy)
        } else {
            Ok(())
        }
    }
}

impl ScheduleCandidate {
    fn is_due(self, now: u64, policy: VerificationPolicy) -> bool {
        is_due(
            self.last_verified_at,
            policy.verification_frequency_seconds,
            now,
        ) || self.drill_is_due(now, policy)
    }

    fn drill_is_due(self, now: u64, policy: VerificationPolicy) -> bool {
        is_due(self.last_drilled_at, policy.drill_frequency_seconds, now)
    }
}

fn is_due(last_success: Option<u64>, frequency: u64, now: u64) -> bool {
    last_success.is_none_or(|last| now.saturating_sub(last) >= frequency)
}
