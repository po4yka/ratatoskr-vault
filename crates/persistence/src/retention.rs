//! Durable retention pins, deletion plans, stages, and audit queries.

mod evaluation;
mod stages;

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::retention::{DEFAULT_RETENTION_POLICY, PinSource};
use ratatoskr_vault_core::target_state::TargetStatus;
use uuid::Uuid;

use crate::Database;

pub use self::evaluation::{
    RetentionCandidateClassification, RetentionCandidateEvidence, RetentionEvaluationMode,
    RetentionEvaluationOutcome, RetentionEvaluationRequest,
};
pub use self::stages::{DeletionStageKind, StageClaimOutcome, StageClaimRequest};

/// Stable cursor for finite retention-audit pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionAuditCursor {
    /// Audit timestamp as UTC Unix seconds.
    pub occurred_at_epoch_seconds: i64,
    /// UUID tie-breaker within one timestamp.
    pub audit_id: Uuid,
}

/// One queryable append-only retention/deletion audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionAuditRecord {
    /// Durable audit identity.
    pub audit_id: Uuid,
    /// Target whose decision or effect this records.
    pub target_id: Uuid,
    /// Snapshot when the record is snapshot-specific.
    pub snapshot_id: Option<Uuid>,
    /// Bounded event vocabulary.
    pub event_kind: String,
    /// Bounded decision/effect reason.
    pub reason: String,
    /// Bounded terminal/current outcome.
    pub outcome: String,
    /// Causal delivery/evaluation identity.
    pub correlation_id: Uuid,
    /// Database observation time as UTC Unix seconds.
    pub occurred_at_epoch_seconds: i64,
}

/// Inputs for one snapshot-level tombstoned deletion intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionPlanRequest {
    /// Selecting immutable retention evaluation.
    pub evaluation_id: Uuid,
    /// Target owning the snapshot.
    pub target_id: Uuid,
    /// Snapshot whose bytes may eventually be removed.
    pub snapshot_id: Uuid,
    /// Optional inactive-target tombstone.
    pub tombstone_id: Option<Uuid>,
    /// Whether policy rather than a direct operator created the plan.
    pub automatic: bool,
    /// Snapshot tombstone time as UTC Unix seconds.
    pub tombstoned_at_epoch_seconds: u64,
    /// Immutable earliest execution time as UTC Unix seconds.
    pub not_before_epoch_seconds: u64,
    /// Finite estimated physical bytes.
    pub estimated_bytes: u64,
    /// Correlation identity for the decision.
    pub correlation_id: Uuid,
}

/// Result of atomically attempting to create one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionPlanOutcome {
    /// One executable-after-grace plan was created.
    Created(Uuid),
    /// An active pin protected the snapshot and refusal evidence was appended.
    ProtectedPinned,
}

pub(crate) async fn apply_target_retention_transition(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_id: Uuid,
    target_pinned: bool,
    to_status: TargetStatus,
    governing: &ValidatedDelivery,
    policy_revision: i64,
    correlation_id: Uuid,
) -> Result<(), VaultError> {
    match to_status {
        TargetStatus::Excluded => {
            if governing.preservation_level != "none" {
                return Err(VaultError::InvalidDelivery {
                    field: "tombstone_policy",
                });
            }
            if target_pinned || governing.pinned == Some(true) {
                return Err(VaultError::InvalidDelivery {
                    field: "target_pin",
                });
            }
            create_target_tombstone(transaction, target_id, policy_revision, correlation_id).await
        }
        TargetStatus::Requested if governing.preservation_level != "none" => {
            cancel_target_tombstone(transaction, target_id, correlation_id).await
        }
        _ => Ok(()),
    }
}

impl Database {
    /// Creates or returns one active source-scoped snapshot pin and appends its audit event.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unknown snapshot or invalid reference and storage failure
    /// when the transaction cannot commit.
    pub async fn pin_snapshot(
        &self,
        snapshot_id: Uuid,
        source: PinSource,
        source_reference: &str,
        correlation_id: Uuid,
    ) -> Result<Uuid, VaultError> {
        if !(1..=255).contains(&source_reference.len()) {
            return Err(VaultError::InvalidDelivery {
                field: "pin_source_reference",
            });
        }
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let target_id: Option<Uuid> = sqlx::query_scalar(
            "select target_id from git_vault.snapshots where snapshot_id = $1 for update",
        )
        .bind(snapshot_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let target_id = target_id.ok_or(VaultError::InvalidDelivery {
            field: "snapshot_id",
        })?;
        let proposed = Uuid::now_v7();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "insert into git_vault.snapshot_pins
                 (pin_id, snapshot_id, source, source_reference, correlation_id, pinned_at)
             values ($1, $2, $3, $4, $5, now())
             on conflict (snapshot_id, source, source_reference) where revoked_at is null
             do nothing
             returning pin_id",
        )
        .bind(proposed)
        .bind(snapshot_id)
        .bind(source.as_str())
        .bind(source_reference)
        .bind(correlation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let pin_id = match inserted {
            Some(pin_id) => {
                append_audit(
                    &mut transaction,
                    AuditEntry {
                        target_id,
                        snapshot_id: Some(snapshot_id),
                        evaluation_id: None,
                        plan_id: None,
                        event_kind: "pin",
                        reason: source.as_str(),
                        outcome: "active",
                        correlation_id,
                    },
                )
                .await?;
                pin_id
            }
            None => sqlx::query_scalar(
                "select pin_id from git_vault.snapshot_pins
                 where snapshot_id = $1 and source = $2 and source_reference = $3
                   and revoked_at is null",
            )
            .bind(snapshot_id)
            .bind(source.as_str())
            .bind(source_reference)
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_failure)?,
        };
        transaction.commit().await.map_err(storage_failure)?;
        Ok(pin_id)
    }

    /// Revokes one active pin exactly once and appends the unpin audit event.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unknown/already-revoked pin and storage failure when the
    /// transaction cannot commit.
    pub async fn revoke_snapshot_pin(
        &self,
        pin_id: Uuid,
        correlation_id: Uuid,
    ) -> Result<(), VaultError> {
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
            "select pins.snapshot_id, snapshots.target_id, pins.source
             from git_vault.snapshot_pins pins
             join git_vault.snapshots using (snapshot_id)
             where pins.pin_id = $1 and pins.revoked_at is null
             for update of pins, snapshots",
        )
        .bind(pin_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        let Some((snapshot_id, target_id, source)) = row else {
            return Err(VaultError::InvalidDelivery { field: "pin_id" });
        };
        sqlx::query(
            "update git_vault.snapshot_pins
             set revoked_at = now(), revocation_correlation_id = $2
             where pin_id = $1",
        )
        .bind(pin_id)
        .bind(correlation_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        append_audit(
            &mut transaction,
            AuditEntry {
                target_id,
                snapshot_id: Some(snapshot_id),
                evaluation_id: None,
                plan_id: None,
                event_kind: "unpin",
                reason: &source,
                outcome: "revoked",
                correlation_id,
            },
        )
        .await?;
        transaction.commit().await.map_err(storage_failure)
    }

    /// Attempts to create an automatic deletion plan.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for inconsistent identities/deadlines and storage failure when the
    /// transaction cannot commit.
    pub async fn create_deletion_plan(
        &self,
        request: DeletionPlanRequest,
    ) -> Result<DeletionPlanOutcome, VaultError> {
        let validated = validate_deletion_plan_request(request)?;
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        lock_deletion_snapshot(&mut transaction, request).await?;
        if request.automatic && snapshot_is_pinned(&mut transaction, request.snapshot_id).await? {
            append_audit(
                &mut transaction,
                AuditEntry {
                    target_id: request.target_id,
                    snapshot_id: Some(request.snapshot_id),
                    evaluation_id: Some(request.evaluation_id),
                    plan_id: None,
                    event_kind: "refusal",
                    reason: "protected_pinned",
                    outcome: "refused",
                    correlation_id: request.correlation_id,
                },
            )
            .await?;
            transaction.commit().await.map_err(storage_failure)?;
            return Ok(DeletionPlanOutcome::ProtectedPinned);
        }

        let plan_id = Uuid::now_v7();
        let reason = deletion_plan_reason(request);
        insert_deletion_plan(&mut transaction, plan_id, request, validated, reason).await?;
        append_audit(
            &mut transaction,
            AuditEntry {
                target_id: request.target_id,
                snapshot_id: Some(request.snapshot_id),
                evaluation_id: Some(request.evaluation_id),
                plan_id: Some(plan_id),
                event_kind: "plan",
                reason,
                outcome: "planned",
                correlation_id: request.correlation_id,
            },
        )
        .await?;
        transaction.commit().await.map_err(storage_failure)?;
        Ok(DeletionPlanOutcome::Created(plan_id))
    }

    /// Reads a finite stable page of one snapshot's retention/deletion history.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unsafe limit and storage failure when the query fails.
    pub async fn snapshot_retention_audit(
        &self,
        snapshot_id: Uuid,
        after: Option<RetentionAuditCursor>,
        limit: u32,
    ) -> Result<Vec<RetentionAuditRecord>, VaultError> {
        validate_audit_limit(limit)?;
        let after_epoch = after.map(|cursor| cursor.occurred_at_epoch_seconds);
        let after_id = after.map(|cursor| cursor.audit_id);
        let rows: Vec<AuditRow> = sqlx::query_as(
            "select audit_id, target_id, snapshot_id, event_kind, reason, outcome,
                    correlation_id, extract(epoch from occurred_at)::bigint
             from git_vault.retention_audit
             where snapshot_id = $1
               and ($2::bigint is null
                    or (occurred_at, audit_id) > (to_timestamp($2), $3))
             order by occurred_at, audit_id
             limit $4",
        )
        .bind(snapshot_id)
        .bind(after_epoch)
        .bind(after_id)
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(storage_failure)?;
        Ok(rows.into_iter().map(map_audit_row).collect())
    }

    /// Reads a finite stable page of one target's retention/deletion history.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unsafe limit and storage failure when the query fails.
    pub async fn target_retention_audit(
        &self,
        target_id: Uuid,
        after: Option<RetentionAuditCursor>,
        limit: u32,
    ) -> Result<Vec<RetentionAuditRecord>, VaultError> {
        validate_audit_limit(limit)?;
        let after_epoch = after.map(|cursor| cursor.occurred_at_epoch_seconds);
        let after_id = after.map(|cursor| cursor.audit_id);
        let rows: Vec<AuditRow> = sqlx::query_as(
            "select audit_id, target_id, snapshot_id, event_kind, reason, outcome,
                    correlation_id, extract(epoch from occurred_at)::bigint
             from git_vault.retention_audit
             where target_id = $1
               and ($2::bigint is null
                    or (occurred_at, audit_id) > (to_timestamp($2), $3))
             order by occurred_at, audit_id
             limit $4",
        )
        .bind(target_id)
        .bind(after_epoch)
        .bind(after_id)
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(storage_failure)?;
        Ok(rows.into_iter().map(map_audit_row).collect())
    }
}

type AuditRow = (Uuid, Uuid, Option<Uuid>, String, String, String, Uuid, i64);

fn map_audit_row(
    (audit_id, target_id, snapshot_id, event_kind, reason, outcome, correlation_id, occurred_at):
        AuditRow,
) -> RetentionAuditRecord {
    RetentionAuditRecord {
        audit_id,
        target_id,
        snapshot_id,
        event_kind,
        reason,
        outcome,
        correlation_id,
        occurred_at_epoch_seconds: occurred_at,
    }
}

fn validate_audit_limit(limit: u32) -> Result<(), VaultError> {
    if (1..=1_000).contains(&limit) {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "retention_audit_limit",
        })
    }
}

async fn create_target_tombstone(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_id: Uuid,
    policy_revision: i64,
    correlation_id: Uuid,
) -> Result<(), VaultError> {
    let grace_seconds = i64::try_from(DEFAULT_RETENTION_POLICY.grace_seconds).map_err(|_| {
        VaultError::InvalidDelivery {
            field: "retention_grace",
        }
    })?;
    let tombstone_id: Option<Uuid> = sqlx::query_scalar(
        "insert into git_vault.tombstones
             (tombstone_id, target_id, governing_policy_revision, reason, was_pinned,
              correlation_id, recorded_at, not_before)
         values ($1, $2, $3, 'policy_inactive', false, $4,
                 now(), now() + ($5 * interval '1 second'))
         on conflict (target_id)
             where cancelled_at is null and completed_at is null
         do nothing
         returning tombstone_id",
    )
    .bind(Uuid::now_v7())
    .bind(target_id)
    .bind(policy_revision)
    .bind(correlation_id)
    .bind(grace_seconds)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if let Some(tombstone_id) = tombstone_id {
        append_audit(
            transaction,
            AuditEntry {
                target_id,
                snapshot_id: None,
                evaluation_id: None,
                plan_id: None,
                event_kind: "tombstone",
                reason: "policy_inactive",
                outcome: "active",
                correlation_id,
            },
        )
        .await?;
        tracing::debug!(%target_id, %tombstone_id, "target tombstone recorded");
    }
    Ok(())
}

async fn cancel_target_tombstone(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), VaultError> {
    let active: Option<Uuid> = sqlx::query_scalar(
        "select tombstone_id from git_vault.tombstones
         where target_id = $1 and cancelled_at is null and completed_at is null
         for update",
    )
    .bind(target_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    let Some(tombstone_id) = active else {
        return Ok(());
    };
    let stage_started: bool = sqlx::query_scalar(
        "select exists(
             select 1 from git_vault.deletion_stage_attempts stage
             join git_vault.deletion_plans plan using (plan_id)
             where plan.tombstone_id = $1
         )",
    )
    .bind(tombstone_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if stage_started {
        return Err(VaultError::InvalidDelivery {
            field: "reactivation_after_deletion_started",
        });
    }
    sqlx::query(
        "update git_vault.deletion_plans
         set status = 'cancelled', cancelled_at = now()
         where tombstone_id = $1 and automatic and status = 'planned'",
    )
    .bind(tombstone_id)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    sqlx::query("update git_vault.tombstones set cancelled_at = now() where tombstone_id = $1")
        .bind(tombstone_id)
        .execute(&mut **transaction)
        .await
        .map_err(storage_failure)?;
    append_audit(
        transaction,
        AuditEntry {
            target_id,
            snapshot_id: None,
            evaluation_id: None,
            plan_id: None,
            event_kind: "tombstone",
            reason: "policy_reactivated",
            outcome: "cancelled",
            correlation_id,
        },
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct ValidatedDeletionPlan {
    tombstoned_at: i64,
    not_before: i64,
    estimated_bytes: i64,
}

fn validate_deletion_plan_request(
    request: DeletionPlanRequest,
) -> Result<ValidatedDeletionPlan, VaultError> {
    let tombstoned_at = checked_epoch(request.tombstoned_at_epoch_seconds)?;
    let not_before = checked_epoch(request.not_before_epoch_seconds)?;
    let estimated_bytes =
        i64::try_from(request.estimated_bytes).map_err(|_| VaultError::InvalidDelivery {
            field: "estimated_bytes",
        })?;
    if not_before <= tombstoned_at {
        return Err(VaultError::InvalidDelivery {
            field: "deletion_not_before",
        });
    }
    Ok(ValidatedDeletionPlan {
        tombstoned_at,
        not_before,
        estimated_bytes,
    })
}

async fn lock_deletion_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: DeletionPlanRequest,
) -> Result<(), VaultError> {
    let present: bool = sqlx::query_scalar(
        "select exists(
             select 1 from git_vault.snapshots
             where snapshot_id = $1 and target_id = $2
             for update
         )",
    )
    .bind(request.snapshot_id)
    .bind(request.target_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if present {
        Ok(())
    } else {
        Err(VaultError::InvalidDelivery {
            field: "deletion_snapshot",
        })
    }
}

pub(crate) async fn snapshot_is_pinned(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot_id: Uuid,
) -> Result<bool, VaultError> {
    sqlx::query_scalar(
        "select exists(
             select 1 from git_vault.snapshot_pins
             where snapshot_id = $1 and revoked_at is null
         )",
    )
    .bind(snapshot_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)
}

fn deletion_plan_reason(request: DeletionPlanRequest) -> &'static str {
    if !request.automatic {
        "operator_request"
    } else if request.tombstone_id.is_some() {
        "target_inactive"
    } else {
        "ordinary_retention"
    }
}

async fn insert_deletion_plan(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    plan_id: Uuid,
    request: DeletionPlanRequest,
    validated: ValidatedDeletionPlan,
    reason: &str,
) -> Result<(), VaultError> {
    sqlx::query(
        "insert into git_vault.deletion_plans
             (plan_id, evaluation_id, target_id, snapshot_id, tombstone_id, reason,
              automatic, tombstoned_at, not_before, estimated_bytes, correlation_id)
         values ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8), to_timestamp($9), $10, $11)",
    )
    .bind(plan_id)
    .bind(request.evaluation_id)
    .bind(request.target_id)
    .bind(request.snapshot_id)
    .bind(request.tombstone_id)
    .bind(reason)
    .bind(request.automatic)
    .bind(validated.tombstoned_at)
    .bind(validated.not_before)
    .bind(validated.estimated_bytes)
    .bind(request.correlation_id)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(storage_failure)
}

pub(crate) struct AuditEntry<'a> {
    pub(crate) target_id: Uuid,
    pub(crate) snapshot_id: Option<Uuid>,
    pub(crate) evaluation_id: Option<Uuid>,
    pub(crate) plan_id: Option<Uuid>,
    pub(crate) event_kind: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) outcome: &'a str,
    pub(crate) correlation_id: Uuid,
}

pub(crate) async fn append_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry: AuditEntry<'_>,
) -> Result<(), VaultError> {
    sqlx::query(
        "insert into git_vault.retention_audit
             (audit_id, target_id, snapshot_id, evaluation_id, plan_id, event_kind,
              reason, outcome, correlation_id, details, occurred_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .bind(entry.target_id)
    .bind(entry.snapshot_id)
    .bind(entry.evaluation_id)
    .bind(entry.plan_id)
    .bind(entry.event_kind)
    .bind(entry.reason)
    .bind(entry.outcome)
    .bind(entry.correlation_id)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(storage_failure)
}

pub(crate) fn checked_epoch(value: u64) -> Result<i64, VaultError> {
    i64::try_from(value).map_err(|_| VaultError::InvalidDelivery {
        field: "retention_timestamp",
    })
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ignored = write!(output, "{byte:02x}");
    }
    output
}

pub(crate) fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(error = %error, "retention persistence failed");
    drop(error);
    VaultError::StorageFailed
}
