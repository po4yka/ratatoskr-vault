//! Append-only persistence for terminal verification and restore-drill evidence.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::BlobRef;
use uuid::Uuid;

use crate::Database;

/// Closed terminal report outcome stored in `PostgreSQL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceOutcome {
    /// Every required stage passed.
    Passed,
    /// A required stage failed.
    Failed,
}

impl EvidenceOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

/// Complete terminal restore-drill evidence ready for one short transaction.
#[derive(Debug, Clone)]
pub struct StoredRestoreDrillReport {
    /// Stable drill attempt identity.
    pub drill_id: Uuid,
    /// Snapshot restored from its stored artifact.
    pub snapshot_id: Uuid,
    /// Exact manifest used as authority.
    pub manifest: BlobRef,
    /// Terminal outcome.
    pub outcome: EvidenceOutcome,
    /// Stable typed failure class when failed.
    pub failure_class: Option<String>,
    /// Whether complete ref evidence matched.
    pub refs_matched: bool,
    /// Total elapsed milliseconds.
    pub duration_millis: u64,
    /// Per-stage bounded evidence.
    pub stages: serde_json::Value,
    /// Expected manifest ref count.
    pub expected_ref_count: u64,
    /// Restored ref count.
    pub observed_ref_count: u64,
    /// Expected canonical ref-set SHA-256.
    pub expected_ref_set_sha256: String,
    /// Observed canonical ref-set SHA-256.
    pub observed_ref_set_sha256: String,
    /// Structural local-only transport assertion.
    pub network_disabled: bool,
    /// Must remain false for every valid report.
    pub live_mirror_accessed: bool,
}

/// Complete terminal artifact-verification evidence ready for one short transaction.
#[derive(Debug, Clone)]
pub struct StoredVerificationReport {
    /// Stable verification attempt identity.
    pub verification_id: Uuid,
    /// Snapshot whose stored evidence was verified.
    pub snapshot_id: Uuid,
    /// Exact manifest used as authority.
    pub manifest: BlobRef,
    /// Terminal outcome.
    pub outcome: EvidenceOutcome,
    /// Stable typed failure class when failed.
    pub failure_class: Option<String>,
    /// Total elapsed milliseconds.
    pub duration_millis: u64,
    /// Per-stage bounded evidence.
    pub stages: serde_json::Value,
    /// Immutable artifact references checked.
    pub checked_artifacts: serde_json::Value,
    /// Expected manifest ref count.
    pub expected_ref_count: u64,
    /// Expected canonical ref-set SHA-256.
    pub expected_ref_set_sha256: String,
}

/// One snapshot's finite scheduler inputs derived from successful terminal evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationScheduleRecord {
    /// Snapshot eligible for verification.
    pub snapshot_id: Uuid,
    /// Stored full-bundle size used for scratch admission.
    pub bundle_size_bytes: u64,
    /// Latest successful verification completion as Unix seconds.
    pub last_verified_at: Option<u64>,
    /// Latest successful restore-drill completion as Unix seconds.
    pub last_drilled_at: Option<u64>,
}

impl Database {
    /// Resolves the immutable manifest `BlobRef` for one scheduled snapshot.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unknown snapshot and storage failure for malformed evidence.
    pub async fn snapshot_manifest(&self, snapshot_id: Uuid) -> Result<BlobRef, VaultError> {
        let row: Option<(String, Vec<u8>, String, i64)> = sqlx::query_as(
            "select blob_owner, manifest_hash, media_type, size_bytes
             from git_vault.manifests where snapshot_id = $1",
        )
        .bind(snapshot_id)
        .fetch_optional(self.pool())
        .await
        .map_err(storage_failure)?;
        let Some((owner, hash, media_type, size_bytes)) = row else {
            return Err(VaultError::InvalidDelivery {
                field: "snapshot_id",
            });
        };
        Ok(BlobRef {
            owner,
            sha256: encode_digest(&hash)?,
            media_type,
            size_bytes: checked_u64(size_bytes)?,
        })
    }

    /// Commits one terminal artifact-verification report and a failed-report outbox fact.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidDelivery`] for malformed evidence and
    /// [`VaultError::StorageFailed`] when the transaction cannot commit.
    pub async fn record_verification(
        &self,
        report: &StoredVerificationReport,
    ) -> Result<(), VaultError> {
        validate_verification_report(report)?;
        let manifest_hash = decode_digest(&report.manifest.sha256)?;
        let expected_refs_hash = decode_digest(&report.expected_ref_set_sha256)?;
        let duration_millis = checked_i64(report.duration_millis)?;
        let expected_ref_count = checked_i64(report.expected_ref_count)?;
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;

        sqlx::query(
            "insert into git_vault.integrity_checks
                 (check_id, snapshot_id, manifest_hash, outcome, failure_class, duration_millis,
                  stages, checked_artifacts, expected_ref_count, expected_refs_hash,
                  started_at, finished_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                     now() - ($6 * interval '1 millisecond'), now())",
        )
        .bind(report.verification_id)
        .bind(report.snapshot_id)
        .bind(manifest_hash)
        .bind(report.outcome.as_str())
        .bind(&report.failure_class)
        .bind(duration_millis)
        .bind(&report.stages)
        .bind(&report.checked_artifacts)
        .bind(expected_ref_count)
        .bind(expected_refs_hash)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;

        if report.outcome == EvidenceOutcome::Failed {
            let payload = serde_json::json!({
                "verification_id": report.verification_id,
                "snapshot_id": report.snapshot_id,
                "manifest_sha256": report.manifest.sha256,
                "failure_class": report.failure_class,
            });
            sqlx::query(
                "insert into git_vault.outbox
                     (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
                 values ($1, 'vault.snapshot.verification_failed.v1',
                         'snapshot_verification', $2, $3, now())",
            )
            .bind(Uuid::now_v7())
            .bind(report.verification_id)
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(storage_failure)?;
        }

        transaction.commit().await.map_err(storage_failure)
    }

    /// Reads finite scheduler inputs from successful append-only evidence.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when evidence cannot be read or decoded.
    pub async fn verification_schedule_records(
        &self,
    ) -> Result<Vec<VerificationScheduleRecord>, VaultError> {
        let rows: Vec<(Uuid, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
            "select snapshots.snapshot_id, snapshot_artifacts.size_bytes,
                    max(extract(epoch from integrity_checks.finished_at)::bigint)
                        filter (where integrity_checks.outcome = 'passed'),
                    max(extract(epoch from restore_drills.finished_at)::bigint)
                        filter (where restore_drills.outcome = 'passed')
             from git_vault.snapshots
             join git_vault.snapshot_artifacts
               on snapshot_artifacts.snapshot_id = snapshots.snapshot_id
              and snapshot_artifacts.kind = 'git_bundle'
             left join git_vault.integrity_checks
               on integrity_checks.snapshot_id = snapshots.snapshot_id
             left join git_vault.restore_drills
               on restore_drills.snapshot_id = snapshots.snapshot_id
             group by snapshots.snapshot_id, snapshot_artifacts.size_bytes
             order by snapshots.snapshot_id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(storage_failure)?;
        rows.into_iter()
            .map(
                |(snapshot_id, size, verified, drilled)| -> Result<_, VaultError> {
                    Ok(VerificationScheduleRecord {
                        snapshot_id,
                        bundle_size_bytes: checked_u64(size)?,
                        last_verified_at: verified.map(checked_u64).transpose()?,
                        last_drilled_at: drilled.map(checked_u64).transpose()?,
                    })
                },
            )
            .collect()
    }

    /// Commits one terminal drill report and its alert-worthy failure fact atomically.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when validation, insertion, or commit fails.
    pub async fn record_restore_drill(
        &self,
        report: &StoredRestoreDrillReport,
    ) -> Result<(), VaultError> {
        validate_report(report)?;
        let manifest_hash = decode_digest(&report.manifest.sha256)?;
        let expected_refs_hash = decode_digest(&report.expected_ref_set_sha256)?;
        let observed_refs_hash = decode_digest(&report.observed_ref_set_sha256)?;
        let duration_millis = checked_i64(report.duration_millis)?;
        let expected_ref_count = checked_i64(report.expected_ref_count)?;
        let observed_ref_count = checked_i64(report.observed_ref_count)?;
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;

        sqlx::query(
            "insert into git_vault.restore_drills
                 (drill_id, snapshot_id, manifest_hash, outcome, failure_class, refs_matched,
                  lfs_restored, duration_millis, stages, expected_ref_count, observed_ref_count,
                  expected_refs_hash, observed_refs_hash, network_disabled,
                  live_mirror_accessed, started_at, finished_at)
             values ($1, $2, $3, $4, $5, $6, null, $7, $8, $9, $10, $11, $12, $13, $14,
                     now() - ($7 * interval '1 millisecond'), now())",
        )
        .bind(report.drill_id)
        .bind(report.snapshot_id)
        .bind(manifest_hash)
        .bind(report.outcome.as_str())
        .bind(&report.failure_class)
        .bind(report.refs_matched)
        .bind(duration_millis)
        .bind(&report.stages)
        .bind(expected_ref_count)
        .bind(observed_ref_count)
        .bind(expected_refs_hash)
        .bind(observed_refs_hash)
        .bind(report.network_disabled)
        .bind(report.live_mirror_accessed)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;

        if report.outcome == EvidenceOutcome::Failed {
            let payload = serde_json::json!({
                "drill_id": report.drill_id,
                "snapshot_id": report.snapshot_id,
                "manifest_sha256": report.manifest.sha256,
                "failure_class": report.failure_class,
            });
            sqlx::query(
                "insert into git_vault.outbox
                     (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
                 values ($1, 'vault.restore.failed.v1', 'restore_drill', $2, $3, now())",
            )
            .bind(Uuid::now_v7())
            .bind(report.drill_id)
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(storage_failure)?;
        }

        transaction.commit().await.map_err(storage_failure)
    }
}

fn validate_report(report: &StoredRestoreDrillReport) -> Result<(), VaultError> {
    let failure_consistent = match report.outcome {
        EvidenceOutcome::Passed => report.failure_class.is_none() && report.refs_matched,
        EvidenceOutcome::Failed => report
            .failure_class
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
    };
    if !failure_consistent
        || report.manifest.owner != "ratatoskr-vault"
        || report.manifest.media_type != "application/json"
        || !report.stages.is_array()
        || !report.network_disabled
        || report.live_mirror_accessed
    {
        return Err(VaultError::InvalidDelivery {
            field: "restore_drill_report",
        });
    }
    Ok(())
}

fn validate_verification_report(report: &StoredVerificationReport) -> Result<(), VaultError> {
    let failure_consistent = match report.outcome {
        EvidenceOutcome::Passed => report.failure_class.is_none(),
        EvidenceOutcome::Failed => report
            .failure_class
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
    };
    if !failure_consistent
        || report.manifest.owner != "ratatoskr-vault"
        || report.manifest.media_type != "application/json"
        || !report.stages.is_array()
        || !report.checked_artifacts.is_array()
    {
        return Err(VaultError::InvalidDelivery {
            field: "verification_report",
        });
    }
    Ok(())
}

fn checked_i64(value: u64) -> Result<i64, VaultError> {
    i64::try_from(value).map_err(|_| VaultError::InvalidDelivery {
        field: "restore_drill_report",
    })
}

fn checked_u64(value: i64) -> Result<u64, VaultError> {
    u64::try_from(value).map_err(|_| VaultError::StorageFailed)
}

fn decode_digest(hex: &str) -> Result<Vec<u8>, VaultError> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(VaultError::InvalidDelivery {
            field: "restore_drill_report",
        });
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let [high_byte, low_byte] = pair else {
                return Err(VaultError::InvalidDelivery {
                    field: "restore_drill_report",
                });
            };
            let high = hex_value(*high_byte).ok_or(VaultError::InvalidDelivery {
                field: "restore_drill_report",
            })?;
            let low = hex_value(*low_byte).ok_or(VaultError::InvalidDelivery {
                field: "restore_drill_report",
            })?;
            Ok((high << 4) | low)
        })
        .collect()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn encode_digest(bytes: &[u8]) -> Result<String, VaultError> {
    use core::fmt::Write as _;

    if bytes.len() != 32 {
        return Err(VaultError::StorageFailed);
    }
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        write!(encoded, "{byte:02x}").map_err(|_| VaultError::StorageFailed)?;
    }
    Ok(encoded)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err owns the diagnostic error before replacing it with the public taxonomy"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "restore verification persistence query failed");
    VaultError::StorageFailed
}
