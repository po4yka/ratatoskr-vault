//! Durable claims and ordered stage evidence for physical deletion.

use ratatoskr_vault_core::error::VaultError;
use uuid::Uuid;

use crate::Database;
use crate::retention::{AuditEntry, append_audit, encode_hex, snapshot_is_pinned, storage_failure};

/// Ordered deletion-stage kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStageKind {
    /// One local content-addressed artifact.
    Local,
    /// The confined local bare mirror.
    MirrorLocal,
    /// One verified off-host placement.
    Replica,
}

/// Inputs required to claim one bounded storage effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageClaimRequest {
    /// Plan authorizing the effect.
    pub plan_id: Uuid,
    /// Ordered kind.
    pub kind: DeletionStageKind,
    /// Stable bounded key within the plan.
    pub stage_key: String,
    /// Local/replica artifact identity when applicable.
    pub artifact_id: Option<Uuid>,
    /// Replica target for a replica stage.
    pub replica_target_id: Option<Uuid>,
    /// Verified placement for a replica stage.
    pub placement_id: Option<Uuid>,
    /// Worker holding the finite lease.
    pub lease_owner: Uuid,
    /// Positive finite lease duration.
    pub lease_seconds: u64,
}

/// Result of trying to claim one stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageClaimOutcome {
    /// Durable stage and physical-identity claims were created.
    Claimed {
        /// Stage attempt identity.
        attempt_id: Uuid,
        /// Physical-object claim identity.
        claim_id: Uuid,
    },
    /// The immutable deadline has not arrived; no effect may run.
    GraceActive,
    /// A pin was added before the effect claim and still protects the snapshot.
    ProtectedPinned,
    /// Another retained snapshot references this exact physical identity; no delete may run.
    SharedReferenceRetained {
        /// Terminal journal identity proving the suppression.
        attempt_id: Uuid,
    },
}

#[derive(Debug)]
struct StagePlan {
    target_id: Uuid,
    snapshot_id: Uuid,
    correlation_id: Uuid,
    status: String,
    automatic: bool,
    grace_active: bool,
}

impl Database {
    /// Attempts to claim one deletion stage.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unsafe/inconsistent unit and storage failure when the
    /// durable lease transaction cannot commit.
    pub async fn claim_deletion_stage(
        &self,
        request: &StageClaimRequest,
    ) -> Result<StageClaimOutcome, VaultError> {
        let lease_seconds = validate_stage_claim(request)?;
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let plan = load_stage_plan(&mut transaction, request.plan_id).await?;
        if let Some(outcome) = stage_claim_refusal(&mut transaction, request.plan_id, &plan).await?
        {
            transaction.commit().await.map_err(storage_failure)?;
            return Ok(outcome);
        }

        let (stage_kind, identity_kind, identity_key) =
            stage_identity(&mut transaction, plan.snapshot_id, request).await?;
        advance_plan_for_stage(&mut transaction, request, &plan.status).await?;
        abandon_expired_stage(&mut transaction, request, &plan, stage_kind).await?;
        lock_physical_identity(&mut transaction, identity_kind, &identity_key).await?;
        if identity_is_shared(&mut transaction, plan.snapshot_id, request).await? {
            let attempt_id = insert_shared_stage_attempt(
                &mut transaction,
                request,
                stage_kind,
                plan.target_id,
                plan.snapshot_id,
                plan.correlation_id,
            )
            .await?;
            transaction.commit().await.map_err(storage_failure)?;
            return Ok(StageClaimOutcome::SharedReferenceRetained { attempt_id });
        }
        let claim_id = claim_physical_identity(
            &mut transaction,
            request,
            lease_seconds,
            identity_kind,
            &identity_key,
        )
        .await?;
        let attempt_id = Uuid::now_v7();
        insert_stage_attempt(
            &mut transaction,
            request,
            stage_kind,
            lease_seconds,
            attempt_id,
            claim_id,
        )
        .await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                target_id: plan.target_id,
                snapshot_id: Some(plan.snapshot_id),
                evaluation_id: None,
                plan_id: Some(request.plan_id),
                event_kind: "stage",
                reason: stage_kind,
                outcome: "claimed",
                correlation_id: plan.correlation_id,
            },
        )
        .await?;
        transaction.commit().await.map_err(storage_failure)?;
        Ok(StageClaimOutcome::Claimed {
            attempt_id,
            claim_id,
        })
    }
}

async fn lock_physical_identity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    identity_kind: &str,
    identity_key: &str,
) -> Result<(), VaultError> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended(concat($1, ':', $2), 0))")
        .bind(identity_kind)
        .bind(identity_key)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(storage_failure)
}

fn validate_stage_claim(request: &StageClaimRequest) -> Result<i64, VaultError> {
    let lease_seconds =
        i64::try_from(request.lease_seconds).map_err(|_| VaultError::InvalidDelivery {
            field: "deletion_stage_lease",
        })?;
    if (1..=86_400).contains(&lease_seconds) && (1..=255).contains(&request.stage_key.len()) {
        Ok(lease_seconds)
    } else {
        Err(VaultError::InvalidDelivery {
            field: "deletion_stage_claim",
        })
    }
}

async fn load_stage_plan(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    plan_id: Uuid,
) -> Result<StagePlan, VaultError> {
    let row: Option<(Uuid, Uuid, Uuid, String, bool, bool)> = sqlx::query_as(
        "select plan.target_id, plan.snapshot_id, plan.correlation_id, plan.status,
                plan.automatic, clock_timestamp() < plan.not_before
         from git_vault.deletion_plans plan
         join git_vault.snapshots snapshot using (snapshot_id)
         where plan.plan_id = $1
         for update of plan, snapshot",
    )
    .bind(plan_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    let (target_id, snapshot_id, correlation_id, status, automatic, grace_active) =
        row.ok_or(VaultError::InvalidDelivery { field: "plan_id" })?;
    Ok(StagePlan {
        target_id,
        snapshot_id,
        correlation_id,
        status,
        automatic,
        grace_active,
    })
}

async fn stage_claim_refusal(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    plan_id: Uuid,
    plan: &StagePlan,
) -> Result<Option<StageClaimOutcome>, VaultError> {
    let refusal = if plan.grace_active {
        Some(("grace_active", StageClaimOutcome::GraceActive))
    } else if plan.automatic && snapshot_is_pinned(transaction, plan.snapshot_id).await? {
        Some(("protected_pinned", StageClaimOutcome::ProtectedPinned))
    } else {
        None
    };
    let Some((reason, outcome)) = refusal else {
        return Ok(None);
    };
    append_audit(
        transaction,
        AuditEntry {
            target_id: plan.target_id,
            snapshot_id: Some(plan.snapshot_id),
            evaluation_id: None,
            plan_id: Some(plan_id),
            event_kind: "refusal",
            reason,
            outcome: "refused",
            correlation_id: plan.correlation_id,
        },
    )
    .await?;
    Ok(Some(outcome))
}

async fn advance_plan_for_stage(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StageClaimRequest,
    plan_status: &str,
) -> Result<(), VaultError> {
    let expected_status = match request.kind {
        DeletionStageKind::Local | DeletionStageKind::MirrorLocal => {
            if plan_status == "planned" {
                sqlx::query(
                    "update git_vault.deletion_plans
                     set status = 'local_deleting'
                     where plan_id = $1",
                )
                .bind(request.plan_id)
                .execute(&mut **transaction)
                .await
                .map_err(storage_failure)?;
            }
            "local_deleting"
        }
        DeletionStageKind::Replica => "replica_deleting",
    };
    if plan_status == expected_status
        || (plan_status == "planned" && expected_status == "local_deleting")
    {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "deletion_stage_order",
        })
    }
}

async fn identity_is_shared(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot_id: Uuid,
    request: &StageClaimRequest,
) -> Result<bool, VaultError> {
    match request.kind {
        DeletionStageKind::Local => {
            let artifact_id = request.artifact_id.ok_or(VaultError::InvalidDelivery {
                field: "deletion_local_artifact",
            })?;
            sqlx::query_scalar(
                "select exists(
                     select 1
                     from git_vault.snapshot_artifacts selected
                     join git_vault.snapshot_artifacts retained
                       on retained.sha256_hash = selected.sha256_hash
                      and retained.artifact_id <> selected.artifact_id
                     join git_vault.snapshots retained_snapshot
                       on retained_snapshot.snapshot_id = retained.snapshot_id
                     where selected.artifact_id = $1 and selected.snapshot_id = $2
                       and retained_snapshot.snapshot_id <> $2
                       and retained_snapshot.status <> 'deleted'
                 )",
            )
            .bind(artifact_id)
            .bind(snapshot_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(storage_failure)
        }
        DeletionStageKind::Replica => {
            let placement_id = request.placement_id.ok_or(VaultError::InvalidDelivery {
                field: "deletion_replica_placement",
            })?;
            sqlx::query_scalar(
                "select exists(
                     select 1
                     from git_vault.replica_placements selected
                     join git_vault.replica_placements retained
                       on retained.replica_target_id = selected.replica_target_id
                      and retained.object_key = selected.object_key
                      and retained.placement_id <> selected.placement_id
                     join git_vault.snapshot_artifacts retained_artifact
                       on retained_artifact.artifact_id = retained.artifact_id
                     join git_vault.snapshots retained_snapshot
                       on retained_snapshot.snapshot_id = retained_artifact.snapshot_id
                     where selected.placement_id = $1
                       and retained_snapshot.snapshot_id <> $2
                       and retained_snapshot.status <> 'deleted'
                 )",
            )
            .bind(placement_id)
            .bind(snapshot_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(storage_failure)
        }
        DeletionStageKind::MirrorLocal => Ok(false),
    }
}

async fn abandon_expired_stage(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StageClaimRequest,
    plan: &StagePlan,
    stage_kind: &str,
) -> Result<(), VaultError> {
    let result = sqlx::query(
        "update git_vault.deletion_stage_attempts
         set outcome = 'abandoned', failure_class = 'lease_expired', finished_at = now()
         where plan_id = $1 and stage_kind = $2 and stage_key = $3 and outcome = 'running'
           and lease_expires_at <= clock_timestamp()",
    )
    .bind(request.plan_id)
    .bind(stage_kind)
    .bind(&request.stage_key)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if result.rows_affected() > 0 {
        append_audit(
            transaction,
            AuditEntry {
                target_id: plan.target_id,
                snapshot_id: Some(plan.snapshot_id),
                evaluation_id: None,
                plan_id: Some(request.plan_id),
                event_kind: "stage",
                reason: stage_kind,
                outcome: "abandoned",
                correlation_id: plan.correlation_id,
            },
        )
        .await?;
    }
    Ok(())
}

async fn insert_shared_stage_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StageClaimRequest,
    stage_kind: &str,
    target_id: Uuid,
    snapshot_id: Uuid,
    correlation_id: Uuid,
) -> Result<Uuid, VaultError> {
    let ordinal: i32 = sqlx::query_scalar(
        "select coalesce(max(ordinal), -1) + 1
         from git_vault.deletion_stage_attempts where plan_id = $1",
    )
    .bind(request.plan_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    let attempt_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.deletion_stage_attempts
             (attempt_id, plan_id, stage_kind, stage_key, artifact_id, replica_target_id,
              placement_id, ordinal, outcome, lease_owner, lease_expires_at,
              absence_verified, started_at, finished_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, 'shared_reference_retained', $9,
                 now(), false, now(), now())",
    )
    .bind(attempt_id)
    .bind(request.plan_id)
    .bind(stage_kind)
    .bind(&request.stage_key)
    .bind(request.artifact_id)
    .bind(request.replica_target_id)
    .bind(request.placement_id)
    .bind(ordinal)
    .bind(request.lease_owner)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    append_audit(
        transaction,
        AuditEntry {
            target_id,
            snapshot_id: Some(snapshot_id),
            evaluation_id: None,
            plan_id: Some(request.plan_id),
            event_kind: "stage",
            reason: stage_kind,
            outcome: "shared_reference_retained",
            correlation_id,
        },
    )
    .await?;
    Ok(attempt_id)
}

async fn claim_physical_identity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StageClaimRequest,
    lease_seconds: i64,
    identity_kind: &str,
    identity_key: &str,
) -> Result<Uuid, VaultError> {
    sqlx::query(
        "update git_vault.physical_object_claims
         set outcome = 'abandoned', failure_class = 'lease_expired', finished_at = now()
         where identity_kind = $1 and identity_key = $2 and outcome = 'running'
           and lease_expires_at <= clock_timestamp()",
    )
    .bind(identity_kind)
    .bind(identity_key)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    let claim_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.physical_object_claims
             (claim_id, plan_id, identity_kind, identity_key, lease_owner,
              lease_expires_at, started_at)
         values ($1, $2, $3, $4, $5, now() + ($6 * interval '1 second'), now())",
    )
    .bind(claim_id)
    .bind(request.plan_id)
    .bind(identity_kind)
    .bind(identity_key)
    .bind(request.lease_owner)
    .bind(lease_seconds)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    Ok(claim_id)
}

async fn insert_stage_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StageClaimRequest,
    stage_kind: &str,
    lease_seconds: i64,
    attempt_id: Uuid,
    claim_id: Uuid,
) -> Result<(), VaultError> {
    let ordinal: i32 = sqlx::query_scalar(
        "select coalesce(max(ordinal), -1) + 1
         from git_vault.deletion_stage_attempts where plan_id = $1",
    )
    .bind(request.plan_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    sqlx::query(
        "insert into git_vault.deletion_stage_attempts
             (attempt_id, plan_id, stage_kind, stage_key, artifact_id, replica_target_id,
              placement_id, claim_id, ordinal, outcome, lease_owner, lease_expires_at,
              started_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'running', $10,
                 now() + ($11 * interval '1 second'), now())",
    )
    .bind(attempt_id)
    .bind(request.plan_id)
    .bind(stage_kind)
    .bind(&request.stage_key)
    .bind(request.artifact_id)
    .bind(request.replica_target_id)
    .bind(request.placement_id)
    .bind(claim_id)
    .bind(ordinal)
    .bind(request.lease_owner)
    .bind(lease_seconds)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(storage_failure)
}

async fn stage_identity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot_id: Uuid,
    request: &StageClaimRequest,
) -> Result<(&'static str, &'static str, String), VaultError> {
    match request.kind {
        DeletionStageKind::Local => {
            let artifact_id = request.artifact_id.ok_or(VaultError::InvalidDelivery {
                field: "deletion_local_artifact",
            })?;
            if request.replica_target_id.is_some() || request.placement_id.is_some() {
                return Err(VaultError::InvalidDelivery {
                    field: "deletion_local_shape",
                });
            }
            let digest: Option<Vec<u8>> = sqlx::query_scalar(
                "select sha256_hash from git_vault.snapshot_artifacts
                 where artifact_id = $1 and snapshot_id = $2",
            )
            .bind(artifact_id)
            .bind(snapshot_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(storage_failure)?;
            Ok((
                "local",
                "local_digest",
                encode_hex(&digest.ok_or(VaultError::InvalidDelivery {
                    field: "deletion_local_artifact",
                })?),
            ))
        }
        DeletionStageKind::MirrorLocal => {
            if request.artifact_id.is_some()
                || request.replica_target_id.is_some()
                || request.placement_id.is_some()
            {
                return Err(VaultError::InvalidDelivery {
                    field: "deletion_mirror_shape",
                });
            }
            let mirror_path: Option<String> = sqlx::query_scalar(
                "select mirrors.storage_path
                 from git_vault.snapshots
                 join git_vault.mirrors using (mirror_id)
                 where snapshots.snapshot_id = $1",
            )
            .bind(snapshot_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(storage_failure)?;
            Ok((
                "mirror_local",
                "mirror_path",
                mirror_path.ok_or(VaultError::InvalidDelivery {
                    field: "deletion_mirror",
                })?,
            ))
        }
        DeletionStageKind::Replica => {
            let artifact_id = request.artifact_id.ok_or(VaultError::InvalidDelivery {
                field: "deletion_replica_artifact",
            })?;
            let replica_target_id =
                request
                    .replica_target_id
                    .ok_or(VaultError::InvalidDelivery {
                        field: "deletion_replica_target",
                    })?;
            let placement_id = request.placement_id.ok_or(VaultError::InvalidDelivery {
                field: "deletion_replica_placement",
            })?;
            let object_key: Option<String> = sqlx::query_scalar(
                "select placements.object_key
                 from git_vault.replica_placements placements
                 join git_vault.snapshot_artifacts artifacts using (artifact_id)
                 where placements.placement_id = $1 and placements.artifact_id = $2
                   and placements.replica_target_id = $3 and artifacts.snapshot_id = $4",
            )
            .bind(placement_id)
            .bind(artifact_id)
            .bind(replica_target_id)
            .bind(snapshot_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(storage_failure)?;
            Ok((
                "replica",
                "replica_key",
                format!(
                    "{replica_target_id}:{}",
                    object_key.ok_or(VaultError::InvalidDelivery {
                        field: "deletion_replica_placement",
                    })?
                ),
            ))
        }
    }
}
