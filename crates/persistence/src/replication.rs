//! Durable off-host target observations, transfer attempts, and verified placement inventory.

use std::time::Duration;

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::BlobRef;
use uuid::Uuid;

use crate::Database;

type DueReplicationRow = (
    Uuid,
    Uuid,
    Uuid,
    String,
    Vec<u8>,
    String,
    i64,
    Option<String>,
    Option<i64>,
);

/// Credential-free configuration observation persisted for one named target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaTargetObservation {
    /// Identifier minted for the target on first sight.
    pub replica_target_id: Uuid,
    /// Stable configuration map key.
    pub name: String,
    /// Scheme, host, and optional port only; never user information, query, or fragment.
    pub endpoint_origin: String,
    /// S3-compatible bucket.
    pub bucket: String,
    /// Fixed operator-controlled object-key prefix.
    pub key_prefix: String,
    /// Whether this target participates in health convergence.
    pub required: bool,
    /// Whether new work is admitted.
    pub enabled: bool,
}

/// Current verified placement projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReplicaPlacement {
    /// Stable placement identity.
    pub placement_id: Uuid,
    /// Snapshot artifact present remotely.
    pub artifact_id: Uuid,
    /// Replica target holding the object.
    pub replica_target_id: Uuid,
    /// Safe content-derived object key.
    pub object_key: String,
    /// Immutable remote identity.
    pub blob: BlobRef,
    /// Latest successful attempt attached to the projection.
    pub last_attempt_id: Uuid,
    /// Latest successful verification as Unix seconds.
    pub last_verified_at: u64,
}

/// One artifact/replica unit still due for upload or re-verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueReplicationUnit {
    /// Snapshot owning the artifact.
    pub snapshot_id: Uuid,
    /// Artifact to transfer.
    pub artifact_id: Uuid,
    /// Target that must receive it.
    pub replica_target_id: Uuid,
    /// Artifact identity and finite size.
    pub blob: BlobRef,
    /// Existing object key when this is re-verification, absent for first placement.
    pub object_key: Option<String>,
    /// Previous successful verification as Unix seconds.
    pub last_verified_at: Option<u64>,
}

impl Database {
    /// Reads a finite oldest-first window of absent or stale artifact placements.
    ///
    /// # Errors
    ///
    /// Returns storage failure once the database-backed implementation is present and a query
    /// fails.
    pub async fn due_replication_units(
        &self,
        replica_target_id: Uuid,
        verified_before_epoch_seconds: u64,
        limit: u32,
    ) -> Result<Vec<DueReplicationUnit>, VaultError> {
        if limit == 0 {
            return Err(VaultError::InvalidDelivery {
                field: "replication_due_limit",
            });
        }
        let cutoff = i64::try_from(verified_before_epoch_seconds).map_err(|_| {
            VaultError::InvalidDelivery {
                field: "replication_freshness_cutoff",
            }
        })?;
        let rows: Vec<DueReplicationRow> = sqlx::query_as(
            "select snapshots.snapshot_id, artifacts.artifact_id,
                    replica_targets.replica_target_id, artifacts.blob_owner,
                    artifacts.sha256_hash, artifacts.media_type, artifacts.size_bytes,
                    placements.object_key,
                    extract(epoch from placements.last_verified_at)::bigint
             from git_vault.snapshot_artifacts artifacts
             join git_vault.snapshots using (snapshot_id)
             join git_vault.replica_targets
               on replica_targets.replica_target_id = $1 and replica_targets.enabled
             left join git_vault.replica_placements placements
               on placements.artifact_id = artifacts.artifact_id
              and placements.replica_target_id = replica_targets.replica_target_id
             where (placements.placement_id is null
                or placements.last_verified_at < to_timestamp($2))
               and not exists (
                 select 1 from git_vault.deletion_plans deletion_plan
                 where deletion_plan.snapshot_id = artifacts.snapshot_id
                   and deletion_plan.status in ('local_deleting', 'replica_deleting')
             )
             order by placements.last_verified_at asc nulls first,
                      snapshots.created_at, artifacts.artifact_id
             limit $3",
        )
        .bind(replica_target_id)
        .bind(cutoff)
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(storage_failure)?;
        rows.into_iter()
            .map(
                |(
                    snapshot_id,
                    artifact_id,
                    replica_target_id,
                    owner,
                    hash,
                    media_type,
                    size,
                    object_key,
                    last_verified,
                )| {
                    Ok(DueReplicationUnit {
                        snapshot_id,
                        artifact_id,
                        replica_target_id,
                        blob: BlobRef {
                            owner,
                            sha256: encode_digest(&hash)?,
                            media_type,
                            size_bytes: checked_u64(size)?,
                        },
                        object_key,
                        last_verified_at: last_verified.map(checked_u64).transpose()?,
                    })
                },
            )
            .collect()
    }

    /// Records a credential-free target observation idempotently by stable name.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for unsafe or incomplete observation fields and storage failure
    /// for database errors.
    pub async fn observe_replica_target(
        &self,
        observation: &ReplicaTargetObservation,
    ) -> Result<Uuid, VaultError> {
        validate_target(observation)?;
        sqlx::query_scalar(
            "insert into git_vault.replica_targets
                 (replica_target_id, name, endpoint_origin, bucket, key_prefix, required, enabled,
                  first_seen_at, last_seen_at)
             values ($1, $2, $3, $4, $5, $6, $7, now(), now())
             on conflict (name) do update
                 set endpoint_origin = excluded.endpoint_origin,
                     bucket = excluded.bucket,
                     key_prefix = excluded.key_prefix,
                     required = excluded.required,
                     enabled = excluded.enabled,
                     last_seen_at = now()
             returning replica_target_id",
        )
        .bind(observation.replica_target_id)
        .bind(&observation.name)
        .bind(&observation.endpoint_origin)
        .bind(&observation.bucket)
        .bind(&observation.key_prefix)
        .bind(observation.required)
        .bind(observation.enabled)
        .fetch_one(self.pool())
        .await
        .map_err(storage_failure)
    }

    /// Claims one artifact/target unit with a finite exclusive lease.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for a zero lease or unknown unit, and storage failure when another
    /// live claim exists or the database refuses the write.
    pub async fn claim_replication_attempt(
        &self,
        artifact_id: Uuid,
        replica_target_id: Uuid,
        lease_owner: Uuid,
        lease: Duration,
    ) -> Result<Uuid, VaultError> {
        let lease_seconds =
            i64::try_from(lease.as_secs()).map_err(|_| VaultError::InvalidDelivery {
                field: "replication_lease",
            })?;
        if lease_seconds == 0 {
            return Err(VaultError::InvalidDelivery {
                field: "replication_lease",
            });
        }
        let attempt_id = Uuid::now_v7();
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        sqlx::query(
            "update git_vault.replication_attempts
             set outcome = 'abandoned', failure_class = 'lease_expired', finished_at = now()
             where artifact_id = $1 and replica_target_id = $2 and outcome = 'running'
               and lease_expires_at <= now()",
        )
        .bind(artifact_id)
        .bind(replica_target_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "with identity_lock as (
                 select pg_advisory_xact_lock(hashtextextended(concat(
                     'replica_key:', replica_targets.replica_target_id::text, ':',
                     case when replica_targets.key_prefix = '' then ''
                          else replica_targets.key_prefix || '/' end,
                     'sha256/', substr(encode(snapshot_artifacts.sha256_hash, 'hex'), 1, 2),
                     '/', encode(snapshot_artifacts.sha256_hash, 'hex')
                 ), 0))
                 from git_vault.snapshot_artifacts
                 cross join git_vault.replica_targets
                 where snapshot_artifacts.artifact_id = $2
                   and replica_targets.replica_target_id = $3
             )
             insert into git_vault.replication_attempts
                 (attempt_id, artifact_id, replica_target_id, outcome, lease_owner,
                  lease_expires_at, started_at)
             select $1, snapshot_artifacts.artifact_id, replica_targets.replica_target_id,
                    'running', $4, now() + ($5 * interval '1 second'), now()
             from git_vault.snapshot_artifacts
             cross join git_vault.replica_targets
             cross join identity_lock
             where snapshot_artifacts.artifact_id = $2
               and replica_targets.replica_target_id = $3
               and replica_targets.enabled
               and not exists (
                   select 1 from git_vault.deletion_plans deletion_plan
                   where deletion_plan.snapshot_id = snapshot_artifacts.snapshot_id
                     and deletion_plan.status in ('local_deleting', 'replica_deleting')
               )
               and not exists (
                   select 1 from git_vault.physical_object_claims physical_claim
                   where physical_claim.identity_kind = 'replica_key'
                     and physical_claim.identity_key = concat(
                         replica_targets.replica_target_id::text, ':',
                         case when replica_targets.key_prefix = '' then ''
                              else replica_targets.key_prefix || '/' end,
                         'sha256/', substr(encode(snapshot_artifacts.sha256_hash, 'hex'), 1, 2),
                         '/', encode(snapshot_artifacts.sha256_hash, 'hex')
                     )
                     and physical_claim.outcome = 'running'
                     and physical_claim.lease_expires_at > clock_timestamp()
               )
             on conflict do nothing
             returning attempt_id",
        )
        .bind(attempt_id)
        .bind(artifact_id)
        .bind(replica_target_id)
        .bind(lease_owner)
        .bind(lease_seconds)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some(inserted) = inserted else {
            return Err(VaultError::StorageFailed);
        };
        transaction.commit().await.map_err(storage_failure)?;
        Ok(inserted)
    }

    /// Atomically finalizes a successful leased attempt and advances the current placement.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for malformed evidence and storage failure unless exactly one live
    /// owned attempt matches the immutable artifact identity.
    pub async fn record_verified_replica(
        &self,
        attempt_id: Uuid,
        lease_owner: Uuid,
        object_key: &str,
        blob: &BlobRef,
    ) -> Result<StoredReplicaPlacement, VaultError> {
        let hash = decode_digest(&blob.sha256)?;
        let size = i64::try_from(blob.size_bytes).map_err(|_| VaultError::InvalidDelivery {
            field: "replica_size_bytes",
        })?;
        if blob.owner != "ratatoskr-vault" || object_key.is_empty() {
            return Err(VaultError::InvalidDelivery {
                field: "replica_placement",
            });
        }
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let unit: Option<(Uuid, Uuid)> = sqlx::query_as(
            "update git_vault.replication_attempts as attempts
             set outcome = 'succeeded', remote_hash = $3, remote_size_bytes = $4,
                 finished_at = now()
             from git_vault.snapshot_artifacts as artifacts
             where attempts.attempt_id = $1 and attempts.lease_owner = $2
               and attempts.outcome = 'running' and attempts.lease_expires_at > now()
               and artifacts.artifact_id = attempts.artifact_id
               and artifacts.sha256_hash = $3 and artifacts.size_bytes = $4
             returning attempts.artifact_id, attempts.replica_target_id",
        )
        .bind(attempt_id)
        .bind(lease_owner)
        .bind(&hash)
        .bind(size)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some((artifact_id, replica_target_id)) = unit else {
            return Err(VaultError::StorageFailed);
        };
        let placement_id = Uuid::now_v7();
        let row: (Uuid, i64) = sqlx::query_as(
            "insert into git_vault.replica_placements
                 (placement_id, artifact_id, replica_target_id, object_key, sha256_hash,
                  size_bytes, first_placed_at, last_verified_at, last_attempt_id)
             values ($1, $2, $3, $4, $5, $6, now(), now(), $7)
             on conflict (artifact_id, replica_target_id) do update
                 set object_key = excluded.object_key,
                     sha256_hash = excluded.sha256_hash,
                     size_bytes = excluded.size_bytes,
                     last_verified_at = now(),
                     last_attempt_id = excluded.last_attempt_id
             returning placement_id, extract(epoch from last_verified_at)::bigint",
        )
        .bind(placement_id)
        .bind(artifact_id)
        .bind(replica_target_id)
        .bind(object_key)
        .bind(&hash)
        .bind(size)
        .bind(attempt_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        transaction.commit().await.map_err(storage_failure)?;
        Ok(StoredReplicaPlacement {
            placement_id: row.0,
            artifact_id,
            replica_target_id,
            object_key: object_key.to_owned(),
            blob: blob.clone(),
            last_attempt_id: attempt_id,
            last_verified_at: checked_u64(row.1)?,
        })
    }

    /// Finalizes a live attempt as failed while preserving it as terminal evidence.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unbounded class and storage failure for an absent/non-owned
    /// live attempt or database error.
    pub async fn record_replication_failure(
        &self,
        attempt_id: Uuid,
        lease_owner: Uuid,
        failure_class: &str,
    ) -> Result<(), VaultError> {
        if failure_class.is_empty() || failure_class.len() > 64 {
            return Err(VaultError::InvalidDelivery {
                field: "replication_failure_class",
            });
        }
        let result = sqlx::query(
            "update git_vault.replication_attempts
             set outcome = 'failed', failure_class = $3, finished_at = now()
             where attempt_id = $1 and lease_owner = $2 and outcome = 'running'",
        )
        .bind(attempt_id)
        .bind(lease_owner)
        .bind(failure_class)
        .execute(self.pool())
        .await
        .map_err(storage_failure)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VaultError::StorageFailed)
        }
    }
}

fn validate_target(observation: &ReplicaTargetObservation) -> Result<(), VaultError> {
    let valid_name = !observation.name.is_empty() && observation.name.len() <= 63;
    let loopback_http = observation
        .endpoint_origin
        .strip_prefix("http://localhost")
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
        || observation
            .endpoint_origin
            .strip_prefix("http://127.0.0.1")
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
        || observation
            .endpoint_origin
            .strip_prefix("http://[::1]")
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'));
    let valid_origin = (observation.endpoint_origin.starts_with("https://") || loopback_http)
        && !observation.endpoint_origin.contains(['@', '?', '#']);
    let valid_prefix = !observation.key_prefix.starts_with('/')
        && !observation.key_prefix.ends_with('/')
        && !observation.key_prefix.split('/').any(|part| part == "..");
    if valid_name
        && valid_origin
        && !observation.bucket.is_empty()
        && observation.bucket.len() <= 255
        && observation.key_prefix.len() <= 255
        && valid_prefix
    {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "replica_target",
        })
    }
}

fn decode_digest(encoded: &str) -> Result<Vec<u8>, VaultError> {
    if encoded.len() != 64 {
        return Err(VaultError::InvalidDelivery {
            field: "replica_digest",
        });
    }
    let mut bytes = Vec::with_capacity(32);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let [high, low] = pair else {
            return Err(VaultError::InvalidDelivery {
                field: "replica_digest",
            });
        };
        let high = hex_value(*high).ok_or(VaultError::InvalidDelivery {
            field: "replica_digest",
        })?;
        let low = hex_value(*low).ok_or(VaultError::InvalidDelivery {
            field: "replica_digest",
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

fn checked_u64(value: i64) -> Result<u64, VaultError> {
    u64::try_from(value).map_err(|_| VaultError::StorageFailed)
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
    reason = "map_err passes ownership directly and provider diagnostics remain diagnostic-only"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "replica persistence query failed");
    VaultError::StorageFailed
}
