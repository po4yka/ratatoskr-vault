//! Durable projections and terminal writes used by the staged deletion executor.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::snapshot::BlobRef;
use uuid::Uuid;

use crate::Database;

/// One exact verified replica object belonging to a deletion artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionExecutionReplica {
    /// Durable placement identity.
    pub placement_id: Uuid,
    /// Configured replica target identity.
    pub replica_target_id: Uuid,
    /// Exact persisted content-derived object key.
    pub object_key: String,
}

/// One local content-addressed artifact and all of its current replica placements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionExecutionArtifact {
    /// Durable artifact identity.
    pub artifact_id: Uuid,
    /// Exact local/remote byte identity.
    pub blob: BlobRef,
    /// Stable replica-stage order.
    pub replicas: Vec<DeletionExecutionReplica>,
}

/// Bounded snapshot-level plan projection for external execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionExecutionPlan {
    /// Durable plan identity.
    pub plan_id: Uuid,
    /// Owning target.
    pub target_id: Uuid,
    /// Snapshot whose bytes are staged for deletion.
    pub snapshot_id: Uuid,
    /// Target tombstone for inactive-target deletion.
    pub tombstone_id: Option<Uuid>,
    /// Confined relative bare-mirror path, only for target tombstone deletion.
    pub mirror_storage_path: Option<String>,
    /// Current durable plan projection.
    pub status: String,
    /// Stable correlation identity.
    pub correlation_id: Uuid,
    /// Stable local artifact order.
    pub artifacts: Vec<DeletionExecutionArtifact>,
}

/// Closed failure vocabulary for a claimed physical deletion effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStageFailureClass {
    /// The confined local store could not verify and remove the exact blob.
    LocalIo,
    /// The remote object remained present or could not be verified absent.
    RemoteVerification,
    /// No configured adapter exists for the persisted replica target.
    ReplicaUnavailable,
}

impl DeletionStageFailureClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LocalIo => "local_io",
            Self::RemoteVerification => "remote_verification",
            Self::ReplicaUnavailable => "replica_unavailable",
        }
    }
}

type ArtifactRow = (Uuid, String, Vec<u8>, String, i64);
type ReplicaRow = (Uuid, Uuid, String);
type PlanRow = (Uuid, Uuid, Option<Uuid>, Option<String>, String, Uuid);

impl Database {
    /// Reads one finite plan and its exact local/replica identities.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unknown plan and storage failure for query errors.
    pub async fn deletion_execution_plan(
        &self,
        plan_id: Uuid,
    ) -> Result<DeletionExecutionPlan, VaultError> {
        let plan: Option<PlanRow> = sqlx::query_as(
            "select plan.target_id, plan.snapshot_id, plan.tombstone_id,
                        case when plan.tombstone_id is null then null else mirror.storage_path end,
                        plan.status, plan.correlation_id
                 from git_vault.deletion_plans plan
                 join git_vault.snapshots snapshot using (snapshot_id)
                 join git_vault.mirrors mirror using (mirror_id)
                 where plan.plan_id = $1",
        )
        .bind(plan_id)
        .fetch_optional(self.pool())
        .await
        .map_err(storage_failure)?;
        let (target_id, snapshot_id, tombstone_id, mirror_storage_path, status, correlation_id) =
            plan.ok_or(VaultError::InvalidDelivery { field: "plan_id" })?;
        let rows: Vec<ArtifactRow> = sqlx::query_as(
            "select artifact_id, blob_owner, sha256_hash, media_type, size_bytes
             from git_vault.snapshot_artifacts
             where snapshot_id = $1 order by artifact_id",
        )
        .bind(snapshot_id)
        .fetch_all(self.pool())
        .await
        .map_err(storage_failure)?;
        let mut artifacts = Vec::with_capacity(rows.len());
        for (artifact_id, owner, digest, media_type, size_bytes) in rows {
            let replicas: Vec<ReplicaRow> = sqlx::query_as(
                "select placement_id, replica_target_id, object_key
                 from git_vault.replica_placements
                 where artifact_id = $1 order by replica_target_id, placement_id",
            )
            .bind(artifact_id)
            .fetch_all(self.pool())
            .await
            .map_err(storage_failure)?;
            artifacts.push(DeletionExecutionArtifact {
                artifact_id,
                blob: BlobRef {
                    owner,
                    sha256: encode_hex(&digest),
                    media_type,
                    size_bytes: u64::try_from(size_bytes).map_err(|_| {
                        VaultError::InvalidDelivery {
                            field: "deletion_artifact_size",
                        }
                    })?,
                },
                replicas: replicas
                    .into_iter()
                    .map(
                        |(placement_id, replica_target_id, object_key)| DeletionExecutionReplica {
                            placement_id,
                            replica_target_id,
                            object_key,
                        },
                    )
                    .collect(),
            });
        }
        Ok(DeletionExecutionPlan {
            plan_id,
            target_id,
            snapshot_id,
            tombstone_id,
            mirror_storage_path,
            status,
            correlation_id,
            artifacts,
        })
    }

    /// Finalizes one claimed stage and its physical identity claim as verified success.
    ///
    /// # Errors
    ///
    /// Returns storage failure unless the exact running attempt and claim are still owned.
    pub async fn record_deletion_stage_success(
        &self,
        attempt_id: Uuid,
        claim_id: Uuid,
    ) -> Result<(), VaultError> {
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let context: Option<(Uuid, Uuid, Uuid, Uuid, String)> = sqlx::query_as(
            "select plan.plan_id, plan.target_id, plan.snapshot_id, plan.correlation_id,
                    stage.stage_kind
             from git_vault.deletion_stage_attempts stage
             join git_vault.deletion_plans plan using (plan_id)
             where stage.attempt_id = $1 and stage.claim_id = $2
               and stage.outcome = 'running'
             for update of stage, plan",
        )
        .bind(attempt_id)
        .bind(claim_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some((plan_id, target_id, snapshot_id, correlation_id, stage_kind)) = context else {
            return Err(VaultError::StorageFailed);
        };
        sqlx::query(
            "update git_vault.deletion_stage_attempts
             set outcome = 'succeeded', absence_verified = true, finished_at = now()
             where attempt_id = $1 and claim_id = $2 and outcome = 'running'",
        )
        .bind(attempt_id)
        .bind(claim_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        sqlx::query(
            "update git_vault.physical_object_claims
             set outcome = 'completed', finished_at = now()
             where claim_id = $1 and outcome = 'running'",
        )
        .bind(claim_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        append_audit(
            &mut transaction,
            target_id,
            snapshot_id,
            plan_id,
            &stage_kind,
            "succeeded",
            correlation_id,
        )
        .await?;
        transaction.commit().await.map_err(storage_failure)
    }

    /// Finalizes one claimed stage as a durable retryable failure and releases its physical claim.
    ///
    /// # Errors
    ///
    /// Returns storage failure unless the exact running attempt and claim are still owned.
    pub async fn record_deletion_stage_failure(
        &self,
        attempt_id: Uuid,
        claim_id: Uuid,
        failure: DeletionStageFailureClass,
    ) -> Result<(), VaultError> {
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let context: Option<(Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
            "select plan.plan_id, plan.target_id, plan.snapshot_id, plan.correlation_id
             from git_vault.deletion_stage_attempts stage
             join git_vault.deletion_plans plan using (plan_id)
             where stage.attempt_id = $1 and stage.claim_id = $2
               and stage.outcome = 'running'
             for update of stage, plan",
        )
        .bind(attempt_id)
        .bind(claim_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some((plan_id, target_id, snapshot_id, correlation_id)) = context else {
            return Err(VaultError::StorageFailed);
        };
        sqlx::query(
            "update git_vault.deletion_stage_attempts
             set outcome = 'failed', failure_class = $3, finished_at = now()
             where attempt_id = $1 and claim_id = $2 and outcome = 'running'",
        )
        .bind(attempt_id)
        .bind(claim_id)
        .bind(failure.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        sqlx::query(
            "update git_vault.physical_object_claims
             set outcome = 'abandoned', failure_class = $2, finished_at = now()
             where claim_id = $1 and outcome = 'running'",
        )
        .bind(claim_id)
        .bind(failure.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        append_audit(
            &mut transaction,
            target_id,
            snapshot_id,
            plan_id,
            failure.as_str(),
            "failed",
            correlation_id,
        )
        .await?;
        transaction.commit().await.map_err(storage_failure)
    }

    /// Advances a plan only when every local artifact has terminal evidence.
    ///
    /// # Errors
    ///
    /// Returns storage failure when local evidence is incomplete or the update fails.
    pub async fn advance_deletion_plan_to_replicas(&self, plan_id: Uuid) -> Result<(), VaultError> {
        let result = sqlx::query(
            "update git_vault.deletion_plans plan set status = 'replica_deleting'
             where plan.plan_id = $1 and plan.status = 'local_deleting'
               and not exists (
                   select 1 from git_vault.snapshot_artifacts artifact
                   where artifact.snapshot_id = plan.snapshot_id
                     and not exists (
                         select 1 from git_vault.deletion_stage_attempts stage
                         where stage.plan_id = plan.plan_id and stage.stage_kind = 'local'
                           and stage.artifact_id = artifact.artifact_id
                           and stage.outcome in ('succeeded', 'shared_reference_retained')
                     )
               )",
        )
        .bind(plan_id)
        .execute(self.pool())
        .await
        .map_err(storage_failure)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(VaultError::StorageFailed)
        }
    }

    /// Completes plan/snapshot and, for a fully completed target tombstone, target projections.
    ///
    /// # Errors
    ///
    /// Returns storage failure unless all required local and replica terminal evidence exists.
    pub async fn complete_deletion_plan(&self, plan_id: Uuid) -> Result<(), VaultError> {
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let context: Option<(Uuid, Uuid, Option<Uuid>, Uuid)> = sqlx::query_as(
            "select target_id, snapshot_id, tombstone_id, correlation_id
             from git_vault.deletion_plans where plan_id = $1 for update",
        )
        .bind(plan_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some((target_id, snapshot_id, tombstone_id, correlation_id)) = context else {
            return Err(VaultError::InvalidDelivery { field: "plan_id" });
        };
        let result = sqlx::query(
            "update git_vault.deletion_plans
             set status = 'completed', completed_at = now()
             where plan_id = $1 and status = 'replica_deleting'",
        )
        .bind(plan_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        if result.rows_affected() != 1 {
            return Err(VaultError::StorageFailed);
        }
        sqlx::query("update git_vault.snapshots set status = 'deleted' where snapshot_id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_failure)?;
        append_audit(
            &mut transaction,
            target_id,
            snapshot_id,
            plan_id,
            "plan_complete",
            "completed",
            correlation_id,
        )
        .await?;
        if let Some(tombstone_id) = tombstone_id {
            complete_target_if_ready(&mut transaction, target_id, tombstone_id).await?;
        }
        transaction.commit().await.map_err(storage_failure)
    }
}

async fn complete_target_if_ready(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_id: Uuid,
    tombstone_id: Uuid,
) -> Result<(), VaultError> {
    let incomplete: bool = sqlx::query_scalar(
        "select exists(
             select 1 from git_vault.deletion_plans
             where tombstone_id = $1 and status <> 'completed'
         )",
    )
    .bind(tombstone_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if !incomplete {
        sqlx::query("update git_vault.tombstones set completed_at = now() where tombstone_id = $1")
            .bind(tombstone_id)
            .execute(&mut **transaction)
            .await
            .map_err(storage_failure)?;
        sqlx::query(
            "update git_vault.targets set status = 'deleted', updated_at = now()
             where target_id = $1 and status = 'deleting'",
        )
        .bind(target_id)
        .execute(&mut **transaction)
        .await
        .map_err(storage_failure)?;
    }
    Ok(())
}

async fn append_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_id: Uuid,
    snapshot_id: Uuid,
    plan_id: Uuid,
    reason: &str,
    outcome: &str,
    correlation_id: Uuid,
) -> Result<(), VaultError> {
    sqlx::query(
        "insert into git_vault.retention_audit
             (audit_id, target_id, snapshot_id, plan_id, event_kind, reason,
              outcome, correlation_id, details, occurred_at)
         values ($1, $2, $3, $4, 'stage', $5, $6, $7, '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .bind(target_id)
    .bind(snapshot_id)
    .bind(plan_id)
    .bind(reason)
    .bind(outcome)
    .bind(correlation_id)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(storage_failure)
}

fn encode_hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ignored = write!(output, "{byte:02x}");
    }
    output
}

fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(error = %error, "retention execution persistence failed");
    drop(error);
    VaultError::StorageFailed
}
