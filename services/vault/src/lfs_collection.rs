//! Explicit, confined Git LFS collection into immutable content-addressed storage.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_core::snapshot::{LfsEvidence, LfsObjectEvidence};
use ratatoskr_vault_gitrunner::{
    ConfinedPath, LfsOperation, LfsRunConfig, LfsRunner, LfsRunnerError, LfsTransferMode,
};
use uuid::Uuid;

/// Finite trusted settings for LFS acquisition.
#[derive(Debug, Clone)]
pub struct LfsCollectionSettings {
    /// Absolute trusted Git LFS executable.
    pub lfs_binary: PathBuf,
    /// Root containing identifier-derived mirrors.
    pub mirror_root: PathBuf,
    /// Root containing run-owned temporary storage.
    pub work_root: PathBuf,
    /// Maximum transient bytes for one collection.
    pub stage_max_bytes: u64,
    /// Maximum referenced objects for one collection.
    pub max_objects: usize,
    /// Per-operation deadline.
    pub deadline: Duration,
    /// Built-in transfer selected from the validated mirror source.
    pub transfer_mode: LfsTransferMode,
}

/// Complete immutable LFS collection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsCollection {
    /// Run identity used only for durable correlation.
    pub run_id: Uuid,
    /// Canonical evidence ready for the snapshot manifest.
    pub evidence: LfsEvidence,
    /// Bytes observed in transient staging.
    pub staged_bytes: u64,
}

/// Typed fail-closed LFS collection outcome.
#[derive(Debug, thiserror::Error)]
pub enum LfsCollectionError {
    /// No collector was configured for an explicitly LFS-required request.
    #[error("Git LFS collection is not configured")]
    ToolUnavailable,
    /// Configured or derived path escaped owned roots.
    #[error("unsafe Git LFS path")]
    UnsafePath,
    /// The runner could not complete one typed operation.
    #[error("Git LFS runner failed: {0}")]
    Runner(#[from] LfsRunnerError),
    /// Git LFS returned a non-zero status.
    #[error("Git LFS {operation} failed: {excerpt}")]
    ToolFailed {
        /// Fixed operation name.
        operation: &'static str,
        /// Bounded runner diagnostic.
        excerpt: String,
    },
    /// The bounded enumeration contained malformed or excessive data.
    #[error("Git LFS enumeration is invalid or exceeds its object limit")]
    InvalidEnumeration,
    /// Transient bytes exceed the admitted stage ceiling.
    #[error("Git LFS staging exceeds quota")]
    QuotaExceeded,
    /// A required object is absent or disagrees with its OID.
    #[error("Git LFS object is missing or corrupt")]
    MissingOrCorrupt,
    /// Owned storage could not publish verified bytes.
    #[error("Git LFS object publication failed")]
    Storage,
}

/// Collects only when called by an explicitly LFS-enabled lifecycle.
#[derive(Debug, Clone)]
pub struct LfsCollector {
    settings: LfsCollectionSettings,
    store: LocalBlobStore,
}

impl LfsCollector {
    /// Validates finite settings and prepares owned roots.
    ///
    /// # Errors
    ///
    /// Returns a typed path/configuration or storage failure before any child process runs.
    pub fn new(
        settings: LfsCollectionSettings,
        store: LocalBlobStore,
    ) -> Result<Self, LfsCollectionError> {
        if !settings.lfs_binary.is_absolute()
            || !settings.mirror_root.is_absolute()
            || !settings.work_root.is_absolute()
            || settings.stage_max_bytes == 0
            || settings.max_objects == 0
            || settings.deadline.is_zero()
        {
            return Err(LfsCollectionError::UnsafePath);
        }
        std::fs::create_dir_all(&settings.mirror_root)
            .and_then(|()| std::fs::create_dir_all(&settings.work_root))
            .map_err(|_| LfsCollectionError::Storage)?;
        Ok(Self { settings, store })
    }

    /// Conservative bytes added to mirror admission for an explicitly enabled collection.
    #[must_use]
    pub const fn reservation_bytes(&self) -> u64 {
        self.settings.stage_max_bytes
    }

    /// Fetches, enumerates, verifies, and publishes every referenced LFS object.
    ///
    /// # Errors
    ///
    /// Returns a typed runner, quota, enumeration, integrity, path, or storage failure; no
    /// complete collection evidence is returned on any failure.
    pub async fn collect(
        &self,
        mirror_relative: &Path,
    ) -> Result<LfsCollection, LfsCollectionError> {
        let run_id = Uuid::now_v7();
        let source_mirror = ConfinedPath::new(&self.settings.mirror_root, mirror_relative)
            .map_err(|_| LfsCollectionError::UnsafePath)?;
        let run_relative = PathBuf::from("runs").join(run_id.to_string());
        let (mirror, storage_relative) =
            if self.settings.transfer_mode == LfsTransferMode::StandaloneFile {
                let staged_mirror_relative = run_relative.join("mirror.git");
                let staged_mirror =
                    ConfinedPath::new(&self.settings.work_root, &staged_mirror_relative)
                        .map_err(|_| LfsCollectionError::UnsafePath)?;
                copy_tree(source_mirror.as_path(), staged_mirror.as_path())?;
                (staged_mirror, staged_mirror_relative.join("lfs"))
            } else {
                (source_mirror, run_relative.join("lfs"))
            };
        let home_relative = run_relative.join("home");
        let storage = ConfinedPath::new(&self.settings.work_root, &storage_relative)
            .map_err(|_| LfsCollectionError::UnsafePath)?;
        let home = ConfinedPath::new(&self.settings.work_root, &home_relative)
            .map_err(|_| LfsCollectionError::UnsafePath)?;
        let runner = LfsRunner::new(LfsRunConfig {
            lfs_binary: self.settings.lfs_binary.clone(),
            mirror_directory: mirror,
            storage_directory: storage.clone(),
            run_home: home,
            transfer_mode: self.settings.transfer_mode,
            deadline: self.settings.deadline,
            stdout_cap_bytes: 4 * 1024 * 1024,
            stderr_cap_bytes: 256 * 1024,
        });
        let result = self
            .collect_with_runner(run_id, &runner, storage.as_path())
            .await;
        let _ignored = std::fs::remove_dir_all(self.settings.work_root.join(run_relative));
        result
    }

    async fn collect_with_runner(
        &self,
        run_id: Uuid,
        runner: &LfsRunner,
        storage: &Path,
    ) -> Result<LfsCollection, LfsCollectionError> {
        let version = runner.run(LfsOperation::Version).await?;
        if version.exit_code != 0 {
            return Err(tool_failure("version", &version));
        }
        let fetch = runner.run(LfsOperation::FetchAll).await?;
        if fetch.exit_code != 0 {
            return Err(tool_failure("fetch", &fetch));
        }
        let staged_bytes = directory_size(storage)?;
        if staged_bytes > self.settings.stage_max_bytes {
            return Err(LfsCollectionError::QuotaExceeded);
        }
        let listed = runner.run(LfsOperation::ListAllLong).await?;
        if listed.exit_code != 0 {
            return Err(tool_failure("ls-files", &listed));
        }
        let oids = parse_oids(&listed.stdout, self.settings.max_objects)?;
        let mut objects = Vec::with_capacity(oids.len());
        for oid in oids {
            let path = object_path(storage, &oid)?;
            let reference = self
                .store
                .reference_for_file(&path, "application/octet-stream".to_owned())
                .map_err(|_| LfsCollectionError::MissingOrCorrupt)?;
            if reference.sha256 != oid {
                return Err(LfsCollectionError::MissingOrCorrupt);
            }
            let blob = self
                .store
                .publish_file(&reference, &path)
                .map_err(|_| LfsCollectionError::Storage)?;
            objects.push(LfsObjectEvidence { oid, blob });
        }
        let tool_version = String::from_utf8(version.stdout)
            .map_err(|_| LfsCollectionError::ToolFailed {
                operation: "version",
                excerpt: "non-UTF-8 output".to_owned(),
            })?
            .trim()
            .to_owned();
        Ok(LfsCollection {
            run_id,
            evidence: LfsEvidence::new(tool_version, objects),
            staged_bytes,
        })
    }
}

fn tool_failure(
    operation: &'static str,
    outcome: &ratatoskr_vault_gitrunner::RunOutcome,
) -> LfsCollectionError {
    LfsCollectionError::ToolFailed {
        operation,
        excerpt: ratatoskr_vault_gitrunner::outcome::bounded_excerpt(&outcome.stderr, 512),
    }
}

fn parse_oids(bytes: &[u8], max_objects: usize) -> Result<Vec<String>, LfsCollectionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| LfsCollectionError::InvalidEnumeration)?;
    let mut unique = BTreeSet::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let oid = line
            .split_whitespace()
            .next()
            .ok_or(LfsCollectionError::InvalidEnumeration)?;
        if oid.len() != 64
            || !oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || !unique.insert(oid.to_owned())
            || unique.len() > max_objects
        {
            return Err(LfsCollectionError::InvalidEnumeration);
        }
    }
    Ok(unique.into_iter().collect())
}

fn object_path(storage: &Path, oid: &str) -> Result<PathBuf, LfsCollectionError> {
    let first: String = oid.chars().take(2).collect();
    let second: String = oid.chars().skip(2).take(2).collect();
    let path = storage.join("objects").join(first).join(second).join(oid);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| LfsCollectionError::MissingOrCorrupt)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(LfsCollectionError::MissingOrCorrupt);
    }
    Ok(path)
}

fn directory_size(directory: &Path) -> Result<u64, LfsCollectionError> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(directory).map_err(|_| LfsCollectionError::Storage)? {
        let entry = entry.map_err(|_| LfsCollectionError::Storage)?;
        let metadata =
            std::fs::symlink_metadata(entry.path()).map_err(|_| LfsCollectionError::Storage)?;
        if metadata.file_type().is_symlink() {
            return Err(LfsCollectionError::UnsafePath);
        }
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else {
            return Err(LfsCollectionError::UnsafePath);
        }
    }
    Ok(total)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), LfsCollectionError> {
    std::fs::create_dir_all(destination).map_err(|_| LfsCollectionError::Storage)?;
    for entry in std::fs::read_dir(source).map_err(|_| LfsCollectionError::Storage)? {
        let entry = entry.map_err(|_| LfsCollectionError::Storage)?;
        let metadata =
            std::fs::symlink_metadata(entry.path()).map_err(|_| LfsCollectionError::Storage)?;
        let target = destination.join(entry.file_name());
        if metadata.file_type().is_symlink() {
            return Err(LfsCollectionError::UnsafePath);
        }
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if metadata.is_file() {
            std::fs::copy(entry.path(), target).map_err(|_| LfsCollectionError::Storage)?;
        } else {
            return Err(LfsCollectionError::UnsafePath);
        }
    }
    Ok(())
}
