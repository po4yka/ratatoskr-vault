//! Append-only persistence for immutable snapshot evidence.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::BlobRef;
use uuid::Uuid;

use crate::Database;

/// The immutable mirror observation from which one snapshot is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSource {
    /// Vault target that owns the mirror.
    pub target_id: Uuid,
    /// The healthy mirror used by Git.
    pub mirror_id: Uuid,
    /// The successful immutable mirror run observed by this snapshot.
    pub mirror_lifecycle_run_id: Uuid,
}

/// The preceding snapshot evidence used to link a later snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotParent {
    /// Prior snapshot identifier.
    pub snapshot_id: Uuid,
    /// Immutable reference to its manifest bytes.
    pub manifest: BlobRef,
}

impl Database {
    /// Validates a healthy source observation and returns its latest manifest, if one exists.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidDelivery`] when the mirror/run is not the target's successful
    /// healthy observation, and [`VaultError::StorageFailed`] for database failures.
    pub async fn snapshot_parent(
        &self,
        source: SnapshotSource,
    ) -> Result<Option<SnapshotParent>, VaultError> {
        ensure_healthy_source(&self.pool, source).await?;
        let row: Option<(Uuid, String, Vec<u8>, String, i64)> = sqlx::query_as(
            "select snapshots.snapshot_id, manifests.blob_owner, manifests.manifest_hash,
                    manifests.media_type, manifests.size_bytes
             from git_vault.snapshots
             join git_vault.manifests using (snapshot_id)
             where snapshots.target_id = $1 and snapshots.mirror_id = $2
             order by snapshots.created_at desc, snapshots.snapshot_id desc
             limit 1",
        )
        .bind(source.target_id)
        .bind(source.mirror_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_failure)?;

        row.map(|(snapshot_id, owner, hash, media_type, size_bytes)| {
            Ok(SnapshotParent {
                snapshot_id,
                manifest: BlobRef {
                    owner,
                    sha256: encode_digest(&hash)?,
                    media_type,
                    size_bytes: u64::try_from(size_bytes).map_err(|_| VaultError::StorageFailed)?,
                },
            })
        })
        .transpose()
    }

    /// Appends one built snapshot and its immutable bundle and manifest references atomically.
    ///
    /// This writes only after both blobs are already immutable in `BlobStore`. A database failure
    /// can therefore leave recoverable unreferenced bytes, never a claimed snapshot without bytes.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidDelivery`] for a stale/unhealthy source or malformed `BlobRef`,
    /// and [`VaultError::StorageFailed`] when the transaction cannot be committed.
    pub async fn record_built_snapshot(
        &self,
        source: SnapshotSource,
        parent_snapshot_id: Option<Uuid>,
        bundle: &BlobRef,
        manifest: &BlobRef,
        refs_hash: &str,
    ) -> Result<Uuid, VaultError> {
        let bundle_hash = decode_digest(&bundle.sha256)?;
        let manifest_hash = decode_digest(&manifest.sha256)?;
        let refs_hash = decode_digest(refs_hash)?;
        validate_blob_ref(bundle)?;
        validate_blob_ref(manifest)?;
        if bundle.media_type != "application/vnd.git.bundle"
            || manifest.media_type != "application/json"
        {
            return Err(VaultError::InvalidDelivery {
                field: "snapshot_blob_ref",
            });
        }
        let bundle_size = checked_size(bundle.size_bytes)?;
        let manifest_size = checked_size(manifest.size_bytes)?;
        let snapshot_id = Uuid::now_v7();
        let mut transaction = self.pool.begin().await.map_err(storage_failure)?;
        ensure_healthy_source(&mut *transaction, source).await?;
        ensure_parent(&mut *transaction, source, parent_snapshot_id).await?;

        sqlx::query(
            "insert into git_vault.snapshots
                 (snapshot_id, target_id, mirror_id, mirror_lifecycle_run_id, parent_snapshot_id,
                  format, status, refs_hash, created_at)
             values ($1, $2, $3, $4, $5, 'git_bundle', 'built', $6, now())",
        )
        .bind(snapshot_id)
        .bind(source.target_id)
        .bind(source.mirror_id)
        .bind(source.mirror_lifecycle_run_id)
        .bind(parent_snapshot_id)
        .bind(refs_hash)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        insert_artifact(
            &mut *transaction,
            snapshot_id,
            "git_bundle",
            bundle,
            bundle_hash,
            bundle_size,
        )
        .await?;
        sqlx::query(
            "insert into git_vault.manifests
                 (manifest_id, snapshot_id, schema_version, manifest_hash, blob_owner,
                  digest_algorithm, media_type, size_bytes, created_at)
             values ($1, $2, 1, $3, $4, 'sha256', $5, $6, now())",
        )
        .bind(Uuid::now_v7())
        .bind(snapshot_id)
        .bind(manifest_hash)
        .bind(&manifest.owner)
        .bind(&manifest.media_type)
        .bind(manifest_size)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        transaction.commit().await.map_err(storage_failure)?;
        Ok(snapshot_id)
    }
}

async fn ensure_healthy_source<'e, E>(executor: E, source: SnapshotSource) -> Result<(), VaultError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let present: bool = sqlx::query_scalar(
        "select exists(
             select 1
             from git_vault.mirrors
             join git_vault.mirror_lifecycle_runs
               on mirror_lifecycle_runs.target_id = mirrors.target_id
             where mirrors.mirror_id = $1 and mirrors.target_id = $2
               and mirrors.status = 'ready' and mirrors.fsck_result = 'ok'
               and mirror_lifecycle_runs.run_id = $3
               and mirror_lifecycle_runs.outcome = 'succeeded'
         )",
    )
    .bind(source.mirror_id)
    .bind(source.target_id)
    .bind(source.mirror_lifecycle_run_id)
    .fetch_one(executor)
    .await
    .map_err(storage_failure)?;
    if present {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "snapshot_source",
        })
    }
}

async fn ensure_parent<'e, E>(
    executor: E,
    source: SnapshotSource,
    parent_snapshot_id: Option<Uuid>,
) -> Result<(), VaultError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let Some(parent_snapshot_id) = parent_snapshot_id else {
        return Ok(());
    };
    let present: bool = sqlx::query_scalar(
        "select exists(
             select 1 from git_vault.snapshots
             where snapshot_id = $1 and target_id = $2 and mirror_id = $3
         )",
    )
    .bind(parent_snapshot_id)
    .bind(source.target_id)
    .bind(source.mirror_id)
    .fetch_one(executor)
    .await
    .map_err(storage_failure)?;
    if present {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "parent_snapshot_id",
        })
    }
}

async fn insert_artifact<'e, E>(
    executor: E,
    snapshot_id: Uuid,
    kind: &'static str,
    reference: &BlobRef,
    hash: Vec<u8>,
    size_bytes: i64,
) -> Result<(), VaultError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "insert into git_vault.snapshot_artifacts
             (artifact_id, snapshot_id, kind, sha256_hash, blob_owner, digest_algorithm,
              media_type, size_bytes, created_at)
         values ($1, $2, $3, $4, $5, 'sha256', $6, $7, now())",
    )
    .bind(Uuid::now_v7())
    .bind(snapshot_id)
    .bind(kind)
    .bind(hash)
    .bind(&reference.owner)
    .bind(&reference.media_type)
    .bind(size_bytes)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(storage_failure)
}

fn validate_blob_ref(reference: &BlobRef) -> Result<(), VaultError> {
    if reference.owner != "ratatoskr-vault" || reference.media_type.is_empty() {
        return Err(VaultError::InvalidDelivery {
            field: "snapshot_blob_ref",
        });
    }
    let _ = decode_digest(&reference.sha256)?;
    let _ = checked_size(reference.size_bytes)?;
    Ok(())
}

fn checked_size(size_bytes: u64) -> Result<i64, VaultError> {
    let converted = i64::try_from(size_bytes).map_err(|_| VaultError::StorageFailed)?;
    if converted > 0 {
        Ok(converted)
    } else {
        Err(VaultError::InvalidDelivery {
            field: "snapshot_blob_ref",
        })
    }
}

fn decode_digest(hex: &str) -> Result<Vec<u8>, VaultError> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(VaultError::InvalidDelivery {
            field: "snapshot_digest",
        });
    }
    let mut digits = hex.bytes();
    let mut bytes = Vec::with_capacity(32);
    while let (Some(high_byte), Some(low_byte)) = (digits.next(), digits.next()) {
        let high = hex_value(high_byte).ok_or(VaultError::InvalidDelivery {
            field: "snapshot_digest",
        })?;
        let low = hex_value(low_byte).ok_or(VaultError::InvalidDelivery {
            field: "snapshot_digest",
        })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn encode_digest(bytes: &[u8]) -> Result<String, VaultError> {
    if bytes.len() != 32 {
        return Err(VaultError::StorageFailed);
    }
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        use core::fmt::Write as _;
        write!(encoded, "{byte:02x}").map_err(|_| VaultError::StorageFailed)?;
    }
    Ok(encoded)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err passes ownership directly and the error is emitted only on the diagnostic channel"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "snapshot persistence query failed");
    VaultError::StorageFailed
}
