//! Append-only terminal Git LFS collection evidence.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::{LfsEvidence, canonical_lfs_digest};
use uuid::Uuid;

use crate::Database;

/// Terminal LFS collection result accepted by persistence.
#[derive(Debug, Clone)]
pub enum LfsCollectionTerminal<'a> {
    /// Exact complete object evidence ready for a signed snapshot manifest.
    Complete(&'a LfsEvidence),
    /// Fail-closed bounded class; a later retry is a separate row.
    Failed {
        /// Stable failure taxonomy.
        failure_class: &'static str,
    },
}

impl Database {
    /// Appends one terminal LFS collector row linked to its successful Git mirror run.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for inconsistent aggregate evidence and storage failure when the
    /// append cannot commit.
    pub async fn record_lfs_collection(
        &self,
        target_id: Uuid,
        mirror_lifecycle_run_id: Uuid,
        terminal: LfsCollectionTerminal<'_>,
    ) -> Result<Uuid, VaultError> {
        let collector_run_id = Uuid::now_v7();
        let (outcome, tool_version, object_count, total_bytes, aggregate_hash, failure_class) =
            match terminal {
                LfsCollectionTerminal::Complete(evidence) => {
                    if evidence.aggregate_sha256 != canonical_lfs_digest(&evidence.objects)
                        || evidence.total_bytes
                            != evidence
                                .objects
                                .iter()
                                .map(|object| object.blob.size_bytes)
                                .sum::<u64>()
                    {
                        return Err(VaultError::InvalidDelivery {
                            field: "lfs_collection_evidence",
                        });
                    }
                    (
                        "complete",
                        Some(evidence.tool_version.as_str()),
                        i64::try_from(evidence.objects.len()).map_err(|_| {
                            VaultError::InvalidDelivery {
                                field: "lfs_collection_evidence",
                            }
                        })?,
                        i64::try_from(evidence.total_bytes).map_err(|_| {
                            VaultError::InvalidDelivery {
                                field: "lfs_collection_evidence",
                            }
                        })?,
                        Some(decode_digest(&evidence.aggregate_sha256)?),
                        None,
                    )
                }
                LfsCollectionTerminal::Failed { failure_class } => {
                    if failure_class.is_empty() || failure_class.len() > 64 {
                        return Err(VaultError::InvalidDelivery {
                            field: "lfs_failure_class",
                        });
                    }
                    ("failed", Some("unknown"), 0, 0, None, Some(failure_class))
                }
            };
        sqlx::query(
            "insert into git_vault.collector_runs
                 (collector_run_id, target_id, collector, outcome, mirror_lifecycle_run_id,
                  tool_version, object_count, total_bytes, aggregate_hash, failure_class, ran_at)
             values ($1, $2, 'git_lfs', $3, $4, $5, $6, $7, $8, $9, now())",
        )
        .bind(collector_run_id)
        .bind(target_id)
        .bind(outcome)
        .bind(mirror_lifecycle_run_id)
        .bind(tool_version)
        .bind(object_count)
        .bind(total_bytes)
        .bind(aggregate_hash)
        .bind(failure_class)
        .execute(self.pool())
        .await
        .map_err(storage_failure)?;
        Ok(collector_run_id)
    }
}

fn decode_digest(hex: &str) -> Result<Vec<u8>, VaultError> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(VaultError::InvalidDelivery {
            field: "lfs_collection_evidence",
        });
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let [high_byte, low_byte] = pair else {
                return Err(VaultError::InvalidDelivery {
                    field: "lfs_collection_evidence",
                });
            };
            let high = hex_value(*high_byte).ok_or(VaultError::InvalidDelivery {
                field: "lfs_collection_evidence",
            })?;
            let low = hex_value(*low_byte).ok_or(VaultError::InvalidDelivery {
                field: "lfs_collection_evidence",
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

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err replaces diagnostic taxonomy"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "LFS collection persistence query failed");
    VaultError::StorageFailed
}
