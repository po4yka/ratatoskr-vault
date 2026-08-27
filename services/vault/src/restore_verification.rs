//! Scheduled verification admission and isolated restore-drill execution.

mod artifact;
mod durable;
mod replica;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ratatoskr_vault_blobstore::{BlobStoreError, LocalBlobStore};
use ratatoskr_vault_core::snapshot::{
    BlobRef, ManifestVerificationKey, RefEvidence, SnapshotManifest, canonical_ref_digest,
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
    /// Required Git LFS bytes were absent, corrupt, or disagreed with the manifest.
    LfsInvalid,
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
    /// Manifest LFS object count when LFS is required.
    pub expected_lfs_object_count: Option<usize>,
    /// Manifest LFS byte total when LFS is required.
    pub expected_lfs_bytes: Option<u64>,
    /// Manifest canonical LFS aggregate digest when required.
    pub expected_lfs_aggregate_sha256: Option<String>,
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
    /// Null for Git-only snapshots; otherwise whether every LFS object was restored.
    pub lfs_restored: Option<bool>,
    /// Manifest LFS object count.
    pub expected_lfs_object_count: Option<usize>,
    /// Materialized verified LFS object count.
    pub observed_lfs_object_count: Option<usize>,
    /// Manifest total LFS bytes.
    pub expected_lfs_bytes: Option<u64>,
    /// Materialized verified LFS bytes.
    pub observed_lfs_bytes: Option<u64>,
    /// Manifest aggregate digest.
    pub expected_lfs_aggregate_sha256: Option<String>,
    /// Recomputed aggregate digest after materialization.
    pub observed_lfs_aggregate_sha256: Option<String>,
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
            return Err((stages, VerificationFailure::BundleInvalid, Vec::new(), None));
        }
        let evidence = Self::load_manifest(store, &verification.manifest)
            .map_err(|failure| (stages.clone(), failure, Vec::new(), None))?;
        let bundle = evidence.bundles.first().ok_or_else(|| {
            (
                stages.clone(),
                VerificationFailure::BundleInvalid,
                Vec::new(),
                None,
            )
        })?;
        store.verify(bundle).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::HashMismatch,
                Vec::new(),
                None,
            )
        })?;
        std::fs::create_dir_all(run_root).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::IsolationFailed,
                Vec::new(),
                None,
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
                    None,
                )
            })?;
        let bundle_path = ConfinedPath::new(store.root(), &relative_bundle).map_err(|_| {
            (
                stages.clone(),
                VerificationFailure::IsolationFailed,
                Vec::new(),
                None,
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
            .map_err(|failure| (stages.clone(), failure, Vec::new(), None))?;
        let compare_started = std::time::Instant::now();
        let matches = observed == evidence.refs;
        stages.push(StageReport {
            stage: "ref_compare",
            passed: matches,
            duration: compare_started.elapsed(),
        });
        if matches {
            let restored_lfs =
                Self::materialize_lfs(store, run_root, evidence.lfs.as_ref(), &mut stages)
                    .map_err(|failure| (stages.clone(), failure, observed.clone(), None))?;
            Ok((stages, observed, restored_lfs))
        } else {
            Err((stages, VerificationFailure::RefMismatch, observed, None))
        }
    }

    fn materialize_lfs(
        store: &LocalBlobStore,
        repository: &std::path::Path,
        evidence: Option<&ratatoskr_vault_core::snapshot::LfsEvidence>,
        stages: &mut Vec<StageReport>,
    ) -> Result<Option<RestoredLfs>, VerificationFailure> {
        let Some(evidence) = evidence else {
            return Ok(None);
        };
        let started = std::time::Instant::now();
        let result = (|| {
            for object in &evidence.objects {
                store
                    .verify(&object.blob)
                    .map_err(|_| VerificationFailure::LfsInvalid)?;
                let source = store
                    .resolve(&object.blob)
                    .map_err(|_| VerificationFailure::LfsInvalid)?;
                let first: String = object.oid.chars().take(2).collect();
                let second: String = object.oid.chars().skip(2).take(2).collect();
                let destination = repository
                    .join("lfs/objects")
                    .join(first)
                    .join(second)
                    .join(&object.oid);
                let parent = destination
                    .parent()
                    .ok_or(VerificationFailure::IsolationFailed)?;
                std::fs::create_dir_all(parent)
                    .map_err(|_| VerificationFailure::IsolationFailed)?;
                let mut input =
                    std::fs::File::open(source).map_err(|_| VerificationFailure::LfsInvalid)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(destination)
                    .map_err(|_| VerificationFailure::IsolationFailed)?;
                std::io::copy(&mut input, &mut output)
                    .map_err(|_| VerificationFailure::IsolationFailed)?;
            }
            Ok(RestoredLfs {
                object_count: evidence.objects.len(),
                total_bytes: evidence.total_bytes,
                aggregate_sha256: ratatoskr_vault_core::snapshot::canonical_lfs_digest(
                    &evidence.objects,
                ),
            })
        })();
        stages.push(StageReport {
            stage: "lfs_objects",
            passed: result.is_ok(),
            duration: started.elapsed(),
        });
        result.map(Some)
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
    (Vec<StageReport>, Vec<RefEvidence>, Option<RestoredLfs>),
    (
        Vec<StageReport>,
        VerificationFailure,
        Vec<RefEvidence>,
        Option<RestoredLfs>,
    ),
>;

#[derive(Debug, Clone)]
struct RestoredLfs {
    object_count: usize,
    total_bytes: u64,
    aggregate_sha256: String,
}

fn build_drill_report(
    drill_id: Uuid,
    verification: &VerificationReport,
    source: RestoreSource,
    started_at: SystemTime,
    duration: Duration,
    result: DrillResult,
) -> RestoreDrillReport {
    let (stages, observed_refs, restored_lfs, failure) = match result {
        Ok((stages, refs, lfs)) => (stages, refs, lfs, None),
        Err((stages, failure, refs, lfs)) => (stages, refs, lfs, Some(failure)),
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
        lfs_restored: verification.expected_lfs_object_count.map(|expected| {
            restored_lfs
                .as_ref()
                .is_some_and(|value| value.object_count == expected)
        }),
        expected_lfs_object_count: verification.expected_lfs_object_count,
        observed_lfs_object_count: restored_lfs.as_ref().map(|value| value.object_count),
        expected_lfs_bytes: verification.expected_lfs_bytes,
        observed_lfs_bytes: restored_lfs.as_ref().map(|value| value.total_bytes),
        expected_lfs_aggregate_sha256: verification.expected_lfs_aggregate_sha256.clone(),
        observed_lfs_aggregate_sha256: restored_lfs.map(|value| value.aggregate_sha256),
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
) -> Result<
    RunOutcome,
    (
        Vec<StageReport>,
        VerificationFailure,
        Vec<RefEvidence>,
        Option<RestoredLfs>,
    ),
> {
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
            None,
        )),
        Ok(_) | Err(_) => Err((
            stages.clone(),
            VerificationFailure::BundleInvalid,
            Vec::new(),
            None,
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
