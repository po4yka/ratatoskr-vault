//! Initial clone and periodic fetch of one confined bare mirror.
//!
//! This module is the only Vault service code that schedules Git. It admits a finite reservation
//! before starting a process, uses the typed runner for every command, publishes a clone only by
//! rename after checks, and appends terminal evidence for every path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatoskr_vault_core::error::{FailureClass, VaultError};
use ratatoskr_vault_core::mirror::{MirrorOperation, MirrorResult};
use ratatoskr_vault_core::target_state::TargetStatus;
use ratatoskr_vault_gitrunner::{
    ConfinedPath, GitOperation, GitRunner, RunConfig, SourceUrl, Subcommand,
};
use ratatoskr_vault_persistence::{Database, LfsCollectionTerminal, QuotaReservationOutcome};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::lfs_collection::{LfsCollection, LfsCollectionError, LfsCollector};

/// Trusted settings for the single-host mirror executor.
#[derive(Debug, Clone)]
pub struct MirrorLifecycleSettings {
    root: PathBuf,
    work_root: PathBuf,
    git_binary: PathBuf,
    per_mirror_max_bytes: u64,
    global_max_bytes: u64,
    admission_started: Option<tokio::sync::mpsc::UnboundedSender<Uuid>>,
    admission_release: Option<tokio::sync::watch::Receiver<bool>>,
}

impl MirrorLifecycleSettings {
    /// Test-only compact settings with finite budgets and the same four-worker cap as production.
    #[must_use]
    pub fn for_test(root: PathBuf, work_root: PathBuf, git_binary: PathBuf) -> Self {
        Self {
            root,
            work_root,
            git_binary,
            per_mirror_max_bytes: 1_000_000,
            global_max_bytes: 4_000_000,
            admission_started: None,
            admission_release: None,
        }
    }

    /// Replaces test settings' finite byte budgets for an admission test.
    #[must_use]
    pub fn with_budgets(mut self, per_mirror_max_bytes: u64, global_max_bytes: u64) -> Self {
        self.per_mirror_max_bytes = per_mirror_max_bytes;
        self.global_max_bytes = global_max_bytes;
        self
    }

    /// Reports and holds admitted test operations at one deterministic point, before Git starts.
    #[must_use]
    pub fn with_admission_test_hooks(
        mut self,
        started: tokio::sync::mpsc::UnboundedSender<Uuid>,
        release: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        self.admission_started = Some(started);
        self.admission_release = Some(release);
        self
    }
}

/// One source and its conservative growth reservation.
#[derive(Debug, Clone)]
pub struct MirrorRequest {
    target_id: Uuid,
    source: String,
    reservation_bytes: u64,
    lfs_enabled: bool,
}

impl MirrorRequest {
    /// Creates a request. The URL is validated before a Git process is constructed.
    #[must_use]
    pub fn new(target_id: Uuid, source: String, reservation_bytes: u64) -> Self {
        Self {
            target_id,
            source,
            reservation_bytes,
            lfs_enabled: false,
        }
    }

    /// Explicitly requires Git LFS collection inside this admitted lifecycle.
    #[must_use]
    pub const fn with_lfs(mut self) -> Self {
        self.lfs_enabled = true;
        self
    }
}

/// The returned terminal outcome, whose detailed evidence is durable in `PostgreSQL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOutcome {
    mirror: MirrorResult,
    lfs_required: bool,
    lfs: Option<LfsCollection>,
    lfs_failure_class: Option<&'static str>,
}

impl LifecycleOutcome {
    /// Whether this run published or refreshed a verified mirror.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.mirror.is_success() && (!self.lfs_required || self.lfs.is_some())
    }

    /// The closed result; detailed diagnostics stay bounded in the durable run evidence.
    #[must_use]
    pub const fn result(&self) -> MirrorResult {
        self.mirror
    }

    /// Complete LFS evidence only when explicitly required and successfully collected.
    #[must_use]
    pub const fn lfs(&self) -> Option<&LfsCollection> {
        self.lfs.as_ref()
    }

    /// Stable typed failure class when required LFS collection did not complete.
    #[must_use]
    pub const fn lfs_failure_class(&self) -> Option<&'static str> {
        self.lfs_failure_class
    }
}

/// Shared lifecycle executor. One semaphore serves clone and fetch alike.
#[derive(Debug, Clone)]
pub struct MirrorLifecycle {
    database: Database,
    settings: MirrorLifecycleSettings,
    permits: Arc<Semaphore>,
    lfs_collector: Option<LfsCollector>,
}

impl MirrorLifecycle {
    /// Builds a lifecycle executor after preparing only its two configured, owned roots.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when the configured roots cannot be created or when
    /// a zero budget would make all work impossible.
    pub fn new(database: Database, settings: MirrorLifecycleSettings) -> Result<Self, VaultError> {
        if settings.per_mirror_max_bytes == 0 || settings.global_max_bytes == 0 {
            return Err(VaultError::StorageFailed);
        }
        std::fs::create_dir_all(&settings.root).map_err(storage_failure)?;
        std::fs::create_dir_all(&settings.work_root).map_err(storage_failure)?;
        Ok(Self {
            database,
            settings,
            permits: Arc::new(Semaphore::new(4)),
            lfs_collector: None,
        })
    }

    /// Attaches the explicitly configured LFS collector used only by LFS-enabled requests.
    #[must_use]
    pub fn with_lfs_collector(mut self, collector: LfsCollector) -> Self {
        self.lfs_collector = Some(collector);
        self
    }

    /// Runs an admitted initial clone or periodic fetch to one terminal durable result.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidDelivery`] for a disallowed source URL and
    /// [`VaultError::StorageFailed`] when admission or terminal evidence cannot be persisted.
    pub async fn run(&self, request: MirrorRequest) -> Result<LifecycleOutcome, VaultError> {
        let (_sender, mut cancellation) = tokio::sync::watch::channel(false);
        self.run_cancellable(request, &mut cancellation).await
    }

    /// Runs a lifecycle request while observing cooperative cancellation.
    ///
    /// The check happens after durable admission and before any Git process starts, which gives a
    /// scheduler a deterministic stop point. Once Git is running its runner process group is
    /// cancellation-safe on future drop; later scheduling integration can signal this receiver
    /// while the runner future is selected.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidDelivery`] for a disallowed source URL and
    /// [`VaultError::StorageFailed`] for database, path, or cancellation-channel failures.
    pub async fn run_cancellable(
        &self,
        request: MirrorRequest,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<LifecycleOutcome, VaultError> {
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| VaultError::StorageFailed)?;
        let source =
            SourceUrl::parse(&request.source).map_err(|_| VaultError::InvalidDelivery {
                field: "source_url",
            })?;
        let published = self.mirror_path(request.target_id)?;
        let operation = if published.exists() {
            MirrorOperation::Fetch
        } else {
            MirrorOperation::InitialClone
        };
        self.transition_started(request.target_id, operation)
            .await?;

        let run_id = Uuid::now_v7();
        let reservation_bytes = self.admitted_reservation(&request);
        let admission = self
            .database
            .reserve_mirror_quota(
                request.target_id,
                run_id,
                reservation_bytes,
                self.settings.per_mirror_max_bytes,
                self.settings.global_max_bytes,
            )
            .await?;
        if admission == QuotaReservationOutcome::QuotaExceeded {
            let result = MirrorResult::QuotaRefused;
            self.finish(request.target_id, run_id, operation, result)
                .await?;
            return Ok(LifecycleOutcome {
                mirror: result,
                lfs_required: request.lfs_enabled,
                lfs: None,
                lfs_failure_class: None,
            });
        }

        if let Some(started) = &self.settings.admission_started {
            let _ignored = started.send(request.target_id);
        }
        if let Some(release) = self.settings.admission_release.as_ref() {
            let mut release = release.clone();
            while !*release.borrow_and_update() {
                if release.changed().await.is_err() {
                    return Err(VaultError::StorageFailed);
                }
            }
        }

        if *cancellation.borrow_and_update() {
            let result = MirrorResult::Interrupted {
                checkpoint: operation.checkpoint(),
            };
            self.finish(request.target_id, run_id, operation, result)
                .await?;
            return Ok(LifecycleOutcome {
                mirror: result,
                lfs_required: request.lfs_enabled,
                lfs: None,
                lfs_failure_class: None,
            });
        }

        let result = match operation {
            MirrorOperation::InitialClone => {
                self.clone_and_verify(request.target_id, run_id, &source, cancellation)
                    .await
            }
            MirrorOperation::Fetch => {
                self.fetch_and_verify(&source, &published, cancellation)
                    .await
            }
        };
        let lfs_result = self.collect_lfs(&request, result).await;
        self.finish(request.target_id, run_id, operation, result)
            .await?;
        let (lfs, lfs_failure_class) = self
            .persist_lfs_result(request.target_id, run_id, lfs_result)
            .await?;
        if request.lfs_enabled && lfs.is_none() && result.is_success() {
            self.database
                .set_target_status(request.target_id, TargetStatus::Degraded)
                .await?;
        }
        Ok(LifecycleOutcome {
            mirror: result,
            lfs_required: request.lfs_enabled,
            lfs,
            lfs_failure_class,
        })
    }

    fn admitted_reservation(&self, request: &MirrorRequest) -> u64 {
        let lfs = request.lfs_enabled.then(|| {
            self.lfs_collector
                .as_ref()
                .map_or(0, LfsCollector::reservation_bytes)
        });
        request.reservation_bytes.saturating_add(lfs.unwrap_or(0))
    }

    async fn collect_lfs(
        &self,
        request: &MirrorRequest,
        result: MirrorResult,
    ) -> Option<Result<LfsCollection, LfsCollectionError>> {
        if !result.is_success() || !request.lfs_enabled {
            return None;
        }
        match &self.lfs_collector {
            Some(collector) => Some(
                collector
                    .collect(Path::new(&mirror_relative_path(request.target_id)))
                    .await,
            ),
            None => Some(Err(LfsCollectionError::ToolUnavailable)),
        }
    }

    async fn persist_lfs_result(
        &self,
        target_id: Uuid,
        run_id: Uuid,
        result: Option<Result<LfsCollection, LfsCollectionError>>,
    ) -> Result<(Option<LfsCollection>, Option<&'static str>), VaultError> {
        match result {
            Some(Ok(collection)) => {
                self.database
                    .record_lfs_collection(
                        target_id,
                        run_id,
                        LfsCollectionTerminal::Complete(&collection.evidence),
                    )
                    .await?;
                Ok((Some(collection), None))
            }
            Some(Err(error)) => {
                let failure_class = lfs_failure_class(&error);
                self.database
                    .record_lfs_collection(
                        target_id,
                        run_id,
                        LfsCollectionTerminal::Failed { failure_class },
                    )
                    .await?;
                Ok((None, Some(failure_class)))
            }
            None => Ok((None, None)),
        }
    }

    /// The identifier-derived published location. Repository names never influence it.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when the configured root cannot confine the path.
    pub fn mirror_path(&self, target_id: Uuid) -> Result<PathBuf, VaultError> {
        let relative = mirror_relative_path(target_id);
        ConfinedPath::new(&self.settings.root, Path::new(&relative))
            .map(|path| path.as_path().to_path_buf())
            .map_err(|_| VaultError::StorageFailed)
    }

    async fn transition_started(
        &self,
        target_id: Uuid,
        operation: MirrorOperation,
    ) -> Result<(), VaultError> {
        let status = match operation {
            MirrorOperation::InitialClone => TargetStatus::Cloning,
            MirrorOperation::Fetch => TargetStatus::Fetching,
        };
        self.database.set_target_status(target_id, status).await
    }

    async fn clone_and_verify(
        &self,
        target_id: Uuid,
        run_id: Uuid,
        source: &SourceUrl,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> MirrorResult {
        let run_relative = format!("runs/{run_id}");
        let run_root = self.settings.work_root.join(&run_relative);
        let stage_relative = format!("{run_relative}/mirror.git");
        if std::fs::create_dir_all(&run_root).is_err() {
            return MirrorResult::Failed {
                failure: FailureClass::DependencyUnavailable,
            };
        }
        let Ok(stage) = ConfinedPath::new(&self.settings.work_root, Path::new(&stage_relative))
        else {
            return MirrorResult::Failed {
                failure: FailureClass::UnsafePath,
            };
        };
        let clone = self.runner(self.settings.work_root.clone());
        let clone_outcome = self
            .run_git(
                clone,
                GitOperation::clone_mirror(source, &stage),
                cancellation,
            )
            .await;
        if matches!(clone_outcome, Err(GitCallFailure::Cancelled)) {
            let _ignored = remove_owned_dir(&run_root);
            return MirrorResult::Interrupted {
                checkpoint: MirrorOperation::InitialClone.checkpoint(),
            };
        }
        if !is_zero_exit(clone_outcome.map_err(|_| ())) {
            let _ignored = remove_owned_dir(&run_root);
            return MirrorResult::Failed {
                failure: FailureClass::RemoteUnavailable,
            };
        }
        let evidence = self.integrity_evidence(stage.as_path(), cancellation).await;
        let result = match evidence {
            Ok(object_count) => match self.publish_staging(target_id, stage.as_path()) {
                Ok(bytes_on_disk) => MirrorResult::Succeeded {
                    object_count,
                    bytes_on_disk,
                },
                Err(_) => MirrorResult::Failed {
                    failure: FailureClass::DependencyUnavailable,
                },
            },
            Err(IntegrityFailure::Broken) => MirrorResult::IntegrityFailed,
            Err(IntegrityFailure::Cancelled) => MirrorResult::Interrupted {
                checkpoint: MirrorOperation::InitialClone.checkpoint(),
            },
            Err(IntegrityFailure::Unavailable) => MirrorResult::Failed {
                failure: FailureClass::DependencyUnavailable,
            },
        };
        let _ignored = remove_owned_dir(&run_root);
        result
    }

    async fn fetch_and_verify(
        &self,
        source: &SourceUrl,
        published: &Path,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> MirrorResult {
        let fetch = self.runner(published.to_path_buf());
        let fetch_outcome = self
            .run_git(fetch, GitOperation::fetch_all(source), cancellation)
            .await;
        if matches!(fetch_outcome, Err(GitCallFailure::Cancelled)) {
            return MirrorResult::Interrupted {
                checkpoint: MirrorOperation::Fetch.checkpoint(),
            };
        }
        if !is_zero_exit(fetch_outcome.map_err(|_| ())) {
            return MirrorResult::Failed {
                failure: FailureClass::RemoteUnavailable,
            };
        }
        match self.integrity_evidence(published, cancellation).await {
            Ok(object_count) => match directory_size(published) {
                Ok(bytes_on_disk) => MirrorResult::Succeeded {
                    object_count,
                    bytes_on_disk,
                },
                Err(_) => MirrorResult::Failed {
                    failure: FailureClass::DependencyUnavailable,
                },
            },
            Err(IntegrityFailure::Broken) => MirrorResult::IntegrityFailed,
            Err(IntegrityFailure::Cancelled) => MirrorResult::Interrupted {
                checkpoint: MirrorOperation::Fetch.checkpoint(),
            },
            Err(IntegrityFailure::Unavailable) => MirrorResult::Failed {
                failure: FailureClass::DependencyUnavailable,
            },
        }
    }

    async fn integrity_evidence(
        &self,
        directory: &Path,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<u64, IntegrityFailure> {
        let runner = self.runner(directory.to_path_buf());
        let fsck = self
            .run_git(runner, GitOperation::fsck_full(), cancellation)
            .await;
        if matches!(fsck, Err(GitCallFailure::Cancelled)) {
            return Err(IntegrityFailure::Cancelled);
        }
        if !is_zero_exit(fsck.map_err(|_| ())) {
            return Err(IntegrityFailure::Broken);
        }
        let refs = self
            .run_git(
                self.runner(directory.to_path_buf()),
                GitOperation::show_ref(),
                cancellation,
            )
            .await
            .map_err(IntegrityFailure::from)?;
        let objects = self
            .run_git(
                self.runner(directory.to_path_buf()),
                GitOperation::rev_list_all_objects(),
                cancellation,
            )
            .await
            .map_err(IntegrityFailure::from)?;
        if refs.exit_code != 0 || objects.exit_code != 0 {
            return Err(IntegrityFailure::Broken);
        }
        let refs = std::str::from_utf8(&refs.stdout).map_err(|_| IntegrityFailure::Broken)?;
        let count = std::str::from_utf8(&objects.stdout)
            .map_err(|_| IntegrityFailure::Broken)?
            .lines()
            .filter(|line| !line.is_empty())
            .count();
        if refs.lines().next().is_none() || count == 0 {
            return Err(IntegrityFailure::Broken);
        }
        u64::try_from(count).map_err(|_| IntegrityFailure::Broken)
    }

    async fn run_git(
        &self,
        runner: GitRunner,
        operation: GitOperation,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<ratatoskr_vault_gitrunner::RunOutcome, GitCallFailure> {
        if *cancellation.borrow_and_update() {
            return Err(GitCallFailure::Cancelled);
        }
        tokio::select! {
            result = runner.run(&operation) => result.map_err(|_| GitCallFailure::Runner),
            changed = cancellation.changed() => match changed {
                Ok(()) if *cancellation.borrow_and_update() => Err(GitCallFailure::Cancelled),
                Ok(()) | Err(_) => runner.run(&operation).await.map_err(|_| GitCallFailure::Runner),
            },
        }
    }

    fn publish_staging(&self, target_id: Uuid, staging: &Path) -> Result<u64, std::io::Error> {
        let published = self
            .mirror_path(target_id)
            .map_err(|_| std::io::Error::other("path"))?;
        let parent = published
            .parent()
            .ok_or_else(|| std::io::Error::other("parent"))?;
        std::fs::create_dir_all(parent)?;
        std::fs::rename(staging, &published)?;
        directory_size(&published)
    }

    async fn finish(
        &self,
        target_id: Uuid,
        run_id: Uuid,
        operation: MirrorOperation,
        result: MirrorResult,
    ) -> Result<(), VaultError> {
        self.database
            .record_mirror_run(target_id, run_id, operation, result)
            .await?;
        match result {
            MirrorResult::Succeeded { bytes_on_disk, .. } => {
                self.database
                    .ensure_mirror(target_id, &mirror_relative_path(target_id))
                    .await?;
                self.database
                    .record_verified_mirror(target_id, bytes_on_disk)
                    .await?;
                self.database
                    .set_target_status(target_id, TargetStatus::Ready)
                    .await
            }
            MirrorResult::IntegrityFailed => {
                self.database.mark_mirror_degraded(target_id).await?;
                self.database
                    .set_target_status(target_id, TargetStatus::Degraded)
                    .await
            }
            _ => {
                self.database
                    .set_target_status(target_id, TargetStatus::Degraded)
                    .await
            }
        }
    }

    fn runner(&self, working_directory: PathBuf) -> GitRunner {
        GitRunner::new(RunConfig {
            git_binary: self.settings.git_binary.clone(),
            allowed: vec![
                Subcommand::CloneMirror,
                Subcommand::Fetch,
                Subcommand::Fsck,
                Subcommand::ShowRef,
                Subcommand::RevList,
            ],
            working_directory,
            run_home: self.settings.work_root.join("runner-home"),
            deadline: Duration::from_mins(1),
            stdout_cap_bytes: 64 * 1024,
            stderr_cap_bytes: 64 * 1024,
            credential_helper: PathBuf::from("/usr/bin/false"),
        })
    }
}

fn lfs_failure_class(error: &LfsCollectionError) -> &'static str {
    match error {
        LfsCollectionError::QuotaExceeded => FailureClass::QuotaExceeded.code(),
        LfsCollectionError::UnsafePath => FailureClass::UnsafePath.code(),
        LfsCollectionError::Runner(ratatoskr_vault_gitrunner::LfsRunnerError::Interrupted) => {
            FailureClass::Interrupted.code()
        }
        LfsCollectionError::Runner(
            ratatoskr_vault_gitrunner::LfsRunnerError::SpawnFailed { .. }
            | ratatoskr_vault_gitrunner::LfsRunnerError::InvalidConfiguration { .. },
        )
        | LfsCollectionError::Storage
        | LfsCollectionError::ToolUnavailable => FailureClass::DependencyUnavailable.code(),
        LfsCollectionError::Runner(ratatoskr_vault_gitrunner::LfsRunnerError::Timeout {
            ..
        }) => FailureClass::RemoteUnavailable.code(),
        LfsCollectionError::Runner(
            ratatoskr_vault_gitrunner::LfsRunnerError::OutputLimitExceeded { .. },
        )
        | LfsCollectionError::ToolFailed { .. }
        | LfsCollectionError::InvalidEnumeration
        | LfsCollectionError::MissingOrCorrupt => FailureClass::LfsIncomplete.code(),
    }
}

#[derive(Debug, Clone, Copy)]
enum IntegrityFailure {
    Broken,
    Cancelled,
    Unavailable,
}

impl From<GitCallFailure> for IntegrityFailure {
    fn from(value: GitCallFailure) -> Self {
        match value {
            GitCallFailure::Cancelled => Self::Cancelled,
            GitCallFailure::Runner => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum GitCallFailure {
    Cancelled,
    Runner,
}

fn is_zero_exit(result: Result<ratatoskr_vault_gitrunner::RunOutcome, ()>) -> bool {
    result.is_ok_and(|outcome| outcome.exit_code == 0)
}

fn mirror_relative_path(target_id: Uuid) -> String {
    let hex = target_id.as_simple().to_string();
    let mut shard = String::with_capacity(2);
    shard.extend(hex.chars().take(2));
    format!("mirrors/{shard}/{}.git", target_id.as_hyphenated())
}

fn directory_size(directory: &Path) -> Result<u64, std::io::Error> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else {
            return Err(std::io::Error::other("mirror contains a non-regular entry"));
        }
    }
    Ok(total)
}

fn remove_owned_dir(path: &Path) -> Result<(), std::io::Error> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err passes ownership directly and the error is emitted only on the diagnostic channel"
)]
fn storage_failure(error: std::io::Error) -> VaultError {
    tracing::warn!(%error, "mirror lifecycle filesystem operation failed");
    VaultError::StorageFailed
}
