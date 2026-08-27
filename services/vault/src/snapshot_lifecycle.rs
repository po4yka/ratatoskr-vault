//! Creation of an immutable local bundle and its manifest from one healthy bare mirror.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_core::snapshot::{BlobRef, RefEvidence, SnapshotManifest};
use ratatoskr_vault_gitrunner::{ConfinedPath, GitOperation, GitRunner, RunConfig, Subcommand};
use ratatoskr_vault_persistence::{Database, SnapshotSource};
use uuid::Uuid;

/// Trusted local settings for snapshot construction.
#[derive(Debug, Clone)]
pub struct SnapshotSettings {
    /// Vault-owned staging root.
    pub work_root: PathBuf,
    /// Trusted system Git binary.
    pub git_binary: PathBuf,
    /// Maximum bytes in one staged snapshot artifact.
    pub max_bytes: u64,
}

/// The source mirror observation selected for one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRequest {
    /// Target owning the mirror.
    pub target_id: Uuid,
    /// Healthy mirror identifier.
    pub mirror_id: Uuid,
    /// Successful mirror lifecycle observation to preserve.
    pub mirror_lifecycle_run_id: Uuid,
}

impl From<SnapshotRequest> for SnapshotSource {
    fn from(value: SnapshotRequest) -> Self {
        Self {
            target_id: value.target_id,
            mirror_id: value.mirror_id,
            mirror_lifecycle_run_id: value.mirror_lifecycle_run_id,
        }
    }
}

/// The immutable references produced by a built snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotResult {
    /// Durable snapshot record identifier.
    pub snapshot_id: Uuid,
    /// Stored bundle reference.
    pub bundle: BlobRef,
    /// Stored JSON manifest reference.
    pub manifest: BlobRef,
    /// Parsed manifest evidence.
    pub evidence: SnapshotManifest,
}

/// A bounded snapshot builder backed by append-only `PostgreSQL` evidence.
#[derive(Debug, Clone)]
pub struct SnapshotLifecycle {
    database: Database,
    settings: SnapshotSettings,
    store: LocalBlobStore,
}

impl SnapshotLifecycle {
    /// Creates a builder after ensuring its owned staging root exists.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when Vault cannot create the owned staging root.
    pub fn new(
        database: Database,
        settings: SnapshotSettings,
        store: LocalBlobStore,
    ) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&settings.work_root)?;
        Ok(Self {
            database,
            settings,
            store,
        })
    }

    /// Creates, publishes, and records a full all-ref bundle and canonical manifest.
    ///
    /// Blob publication precedes the short database transaction. Thus a database failure may
    /// leave unreferenced content-addressed bytes for retention to reap, but never a snapshot row
    /// that claims unavailable bytes.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic error when source selection, confined Git, `BlobStore` publication, or
    /// append-only persistence fails before a snapshot success record is committed.
    pub async fn snapshot(
        &self,
        request: SnapshotRequest,
        mirror: &Path,
    ) -> Result<SnapshotResult, String> {
        let source = SnapshotSource::from(request);
        let parent = self
            .database
            .snapshot_parent(source)
            .await
            .map_err(|error| format!("snapshot source selection failed: {error:?}"))?;
        let run_id = Uuid::now_v7();
        let run_root = self
            .settings
            .work_root
            .join("snapshot-runs")
            .join(run_id.to_string());
        std::fs::create_dir_all(&run_root).map_err(|error| error.to_string())?;
        let result = self
            .build_and_publish(
                mirror,
                &run_root,
                parent.as_ref().map(|value| value.manifest.clone()),
            )
            .await;
        let _ignored = std::fs::remove_dir_all(&run_root);
        let (bundle, manifest, evidence) = result?;
        let snapshot_id = self
            .database
            .record_built_snapshot(
                source,
                parent.map(|value| value.snapshot_id),
                &bundle,
                &manifest,
                &evidence.ref_set_sha256,
            )
            .await
            .map_err(|error| format!("snapshot persistence failed: {error:?}"))?;
        Ok(SnapshotResult {
            snapshot_id,
            bundle,
            manifest,
            evidence,
        })
    }

    async fn build_and_publish(
        &self,
        mirror: &Path,
        run_root: &Path,
        parent_manifest: Option<BlobRef>,
    ) -> Result<(BlobRef, BlobRef, SnapshotManifest), String> {
        let staged_bundle = run_root.join("snapshot.bundle");
        let relative = staged_bundle
            .strip_prefix(&self.settings.work_root)
            .map_err(|_| "snapshot staging escaped work root")?;
        let stage = ConfinedPath::new(&self.settings.work_root, relative)
            .map_err(|error| error.to_string())?;
        let runner = self.runner(mirror.to_path_buf(), run_root);
        let bundle = runner
            .run(&GitOperation::bundle_create(&stage))
            .await
            .map_err(|error| error.to_string())?;
        if bundle.exit_code != 0 {
            return Err("bundle creation failed".to_owned());
        }
        let bundle_metadata =
            std::fs::metadata(stage.as_path()).map_err(|error| error.to_string())?;
        if bundle_metadata.len() == 0 || bundle_metadata.len() > self.settings.max_bytes {
            return Err("bundle exceeds the configured snapshot byte limit".to_owned());
        }
        let refs = runner
            .run(&GitOperation::show_ref())
            .await
            .map_err(|error| error.to_string())?;
        if refs.exit_code != 0 {
            return Err("ref enumeration failed".to_owned());
        }
        let ref_text =
            std::str::from_utf8(&refs.stdout).map_err(|_| "non-UTF-8 refs".to_owned())?;
        let parsed = parse_refs(ref_text)?;
        let bundle_ref = self
            .store
            .reference_for_file(stage.as_path(), "application/vnd.git.bundle".to_owned())
            .map_err(|error| error.to_string())?;
        self.store
            .publish_file(&bundle_ref, stage.as_path())
            .map_err(|error| error.to_string())?;
        let evidence = SnapshotManifest::new(
            parsed,
            vec![bundle_ref.clone()],
            parent_manifest,
            utc_timestamp()?,
        );
        let manifest_path = run_root.join("snapshot-manifest.json");
        let payload = serde_json::to_vec(&evidence).map_err(|error| error.to_string())?;
        std::fs::write(&manifest_path, payload).map_err(|error| error.to_string())?;
        let manifest_ref = self
            .store
            .reference_for_file(&manifest_path, "application/json".to_owned())
            .map_err(|error| error.to_string())?;
        self.store
            .publish_file(&manifest_ref, &manifest_path)
            .map_err(|error| error.to_string())?;
        Ok((bundle_ref, manifest_ref, evidence))
    }

    fn runner(&self, directory: PathBuf, run_root: &Path) -> GitRunner {
        GitRunner::new(RunConfig {
            git_binary: self.settings.git_binary.clone(),
            allowed: vec![Subcommand::Bundle, Subcommand::ShowRef],
            working_directory: directory,
            run_home: run_root.join("runner-home"),
            deadline: Duration::from_mins(1),
            stdout_cap_bytes: 64 * 1024,
            stderr_cap_bytes: 64 * 1024,
            credential_helper: PathBuf::from("/usr/bin/false"),
        })
    }
}

fn parse_refs(output: &str) -> Result<Vec<RefEvidence>, String> {
    let mut names = BTreeSet::new();
    output
        .lines()
        .map(|line| {
            let (oid, name) = line
                .split_once(' ')
                .ok_or_else(|| "malformed show-ref output".to_owned())?;
            let valid_oid = matches!(oid.len(), 40 | 64)
                && oid
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
            if !valid_oid || name.is_empty() || name.bytes().any(|byte| byte.is_ascii_control()) {
                return Err("invalid ref evidence".to_owned());
            }
            if !names.insert(name) {
                return Err("duplicate ref evidence".to_owned());
            }
            Ok(RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            })
        })
        .collect()
}

fn utc_timestamp() -> Result<String, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_secs();
    let days =
        i64::try_from(seconds / 86_400).map_err(|_| "system clock is out of range".to_owned())?;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60
    ))
}

fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}
