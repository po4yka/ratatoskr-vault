//! Immutable policy-engine evaluations and ordered candidate evidence.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::retention::{PinSource, RetentionPolicy};
use uuid::Uuid;

use crate::Database;
use crate::retention::{AuditEntry, append_audit, checked_epoch, storage_failure};

/// Closed trigger for one durable retention evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionEvaluationMode {
    /// Routine policy-window evaluation.
    Scheduled,
    /// Bounded admission response under storage pressure.
    QuotaPressure,
}

/// Closed terminal result for one retention evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionEvaluationOutcome {
    /// One or more snapshots are eligible for a deletion plan.
    Selected,
    /// Every considered snapshot remains protected.
    NoCandidates,
    /// Protected evidence prevents the requested allocation from being released.
    AllocationRefused,
}

/// Closed explanation stored for one ordered candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionCandidateClassification {
    /// An active-target snapshot is due under ordinary policy.
    EligibleOrdinary,
    /// A grace-complete inactive-target snapshot is due in the second pressure tier.
    EligibleInactiveTarget,
    /// An active durable pin protects the snapshot.
    ProtectedPinned,
    /// The snapshot is younger than the configured age floor.
    ProtectedAgeFloor,
    /// The snapshot is within the per-mirror keep-N window.
    ProtectedKeepLastRestorable,
    /// A durable deletion intent exists but its grace deadline is still active.
    GraceActive,
}

/// Complete persisted evidence for one candidate in engine order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCandidateEvidence {
    /// Snapshot being classified.
    pub snapshot_id: Uuid,
    /// Closed engine result.
    pub classification: RetentionCandidateClassification,
    /// Active durable sources observed by the evaluation.
    pub pin_sources: Vec<PinSource>,
    /// Whether the target is inactive and grace-complete.
    pub target_inactive: bool,
    /// Finite physical-byte estimate.
    pub estimated_bytes: u64,
    /// Fixed deletion deadline, when one already exists.
    pub deletion_not_before_epoch_seconds: Option<u64>,
}

/// Atomic immutable inputs and outputs of one policy-engine evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionEvaluationRequest {
    /// Target whose snapshots were evaluated.
    pub target_id: Uuid,
    /// Durable policy identity.
    pub policy_id: Uuid,
    /// Exact policy values used by the engine.
    pub policy: RetentionPolicy,
    /// Scheduled or quota-pressure mode.
    pub mode: RetentionEvaluationMode,
    /// Positive requested bytes only for quota pressure.
    pub required_bytes: Option<u64>,
    /// Terminal evaluation result.
    pub outcome: RetentionEvaluationOutcome,
    /// Stable causal identity.
    pub correlation_id: Uuid,
    /// Evaluation clock value.
    pub evaluated_at_epoch_seconds: u64,
    /// Every considered candidate in deterministic engine order.
    pub candidates: Vec<RetentionCandidateEvidence>,
}

#[derive(Debug, Clone, Copy)]
struct ValidatedEvaluation {
    required_bytes: Option<i64>,
    evaluated_at: i64,
    minimum_age_seconds: i64,
    grace_seconds: i64,
    keep_last_restorable: i32,
}

impl RetentionEvaluationMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::QuotaPressure => "quota_pressure",
        }
    }
}

impl RetentionEvaluationOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::NoCandidates => "no_candidates",
            Self::AllocationRefused => "allocation_refused",
        }
    }
}

impl RetentionCandidateClassification {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EligibleOrdinary => "eligible_ordinary",
            Self::EligibleInactiveTarget => "eligible_inactive_target",
            Self::ProtectedPinned => "protected_pinned",
            Self::ProtectedAgeFloor => "protected_age_floor",
            Self::ProtectedKeepLastRestorable => "protected_keep_last_restorable",
            Self::GraceActive => "grace_active",
        }
    }

    const fn audit_outcome(self) -> &'static str {
        match self {
            Self::EligibleOrdinary | Self::EligibleInactiveTarget => "selected",
            Self::ProtectedPinned
            | Self::ProtectedAgeFloor
            | Self::ProtectedKeepLastRestorable
            | Self::GraceActive => "protected",
        }
    }
}

impl Database {
    /// Persists one complete immutable engine evaluation.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for inconsistent inputs and storage failure when the transaction
    /// cannot commit.
    pub async fn record_retention_evaluation(
        &self,
        request: &RetentionEvaluationRequest,
    ) -> Result<Uuid, VaultError> {
        let validated = validate_evaluation_request(request)?;
        let mut transaction = self.pool().begin().await.map_err(storage_failure)?;
        validate_evaluation_scope(&mut transaction, request, validated).await?;
        let evaluation_id = Uuid::now_v7();
        sqlx::query(
            "insert into git_vault.retention_evaluations
                 (evaluation_id, target_id, policy_id, mode, policy_snapshot, required_bytes,
                  outcome, correlation_id, evaluated_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, to_timestamp($9))",
        )
        .bind(evaluation_id)
        .bind(request.target_id)
        .bind(request.policy_id)
        .bind(request.mode.as_str())
        .bind(serde_json::json!({
            "keep_last_restorable": request.policy.keep_last_restorable,
            "minimum_age_seconds": request.policy.minimum_age_seconds,
            "grace_seconds": request.policy.grace_seconds,
        }))
        .bind(validated.required_bytes)
        .bind(request.outcome.as_str())
        .bind(request.correlation_id)
        .bind(validated.evaluated_at)
        .execute(&mut *transaction)
        .await
        .map_err(storage_failure)?;
        for (ordinal, candidate) in request.candidates.iter().enumerate() {
            insert_evaluation_candidate(
                &mut transaction,
                request,
                evaluation_id,
                i32::try_from(ordinal).map_err(|_| VaultError::InvalidDelivery {
                    field: "retention_candidates",
                })?,
                candidate,
            )
            .await?;
        }
        transaction.commit().await.map_err(storage_failure)?;
        Ok(evaluation_id)
    }
}

fn validate_evaluation_request(
    request: &RetentionEvaluationRequest,
) -> Result<ValidatedEvaluation, VaultError> {
    if request.candidates.len() > 100_000 {
        return Err(VaultError::InvalidDelivery {
            field: "retention_candidates",
        });
    }
    let mut snapshot_ids = std::collections::BTreeSet::new();
    for candidate in &request.candidates {
        if !snapshot_ids.insert(candidate.snapshot_id)
            || !candidate
                .pin_sources
                .windows(2)
                .all(|pins| matches!(pins, [left, right] if left < right))
        {
            return Err(VaultError::InvalidDelivery {
                field: "retention_candidates",
            });
        }
    }
    let required_bytes = match (request.mode, request.required_bytes) {
        (RetentionEvaluationMode::Scheduled, None) => None,
        (RetentionEvaluationMode::QuotaPressure, Some(bytes)) if bytes > 0 => Some(
            i64::try_from(bytes).map_err(|_| VaultError::InvalidDelivery {
                field: "retention_required_bytes",
            })?,
        ),
        _ => {
            return Err(VaultError::InvalidDelivery {
                field: "retention_required_bytes",
            });
        }
    };
    if request.outcome == RetentionEvaluationOutcome::AllocationRefused
        && request.mode != RetentionEvaluationMode::QuotaPressure
    {
        return Err(VaultError::InvalidDelivery {
            field: "retention_evaluation_outcome",
        });
    }
    let keep_last_restorable =
        i32::try_from(request.policy.keep_last_restorable).map_err(|_| {
            VaultError::InvalidDelivery {
                field: "retention_policy",
            }
        })?;
    let validated = ValidatedEvaluation {
        required_bytes,
        evaluated_at: checked_epoch(request.evaluated_at_epoch_seconds)?,
        minimum_age_seconds: checked_epoch(request.policy.minimum_age_seconds)?,
        grace_seconds: checked_epoch(request.policy.grace_seconds)?,
        keep_last_restorable,
    };
    if validated.minimum_age_seconds > 315_360_000
        || !(1..=315_360_000).contains(&validated.grace_seconds)
        || !(1..=1_000_000).contains(&validated.keep_last_restorable)
    {
        return Err(VaultError::InvalidDelivery {
            field: "retention_policy",
        });
    }
    Ok(validated)
}

async fn validate_evaluation_scope(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &RetentionEvaluationRequest,
    validated: ValidatedEvaluation,
) -> Result<(), VaultError> {
    let stored: Option<(i64, i64, i32)> = sqlx::query_as(
        "select policy.minimum_age_seconds, policy.grace_seconds,
                policy.keep_last_restorable
         from git_vault.retention_policies policy
         where policy.policy_id = $1
           and exists (select 1 from git_vault.targets where target_id = $2)",
    )
    .bind(request.policy_id)
    .bind(request.target_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if stored
        != Some((
            validated.minimum_age_seconds,
            validated.grace_seconds,
            validated.keep_last_restorable,
        ))
    {
        return Err(VaultError::InvalidDelivery {
            field: "retention_evaluation_scope",
        });
    }
    Ok(())
}

async fn insert_evaluation_candidate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &RetentionEvaluationRequest,
    evaluation_id: Uuid,
    ordinal: i32,
    candidate: &RetentionCandidateEvidence,
) -> Result<(), VaultError> {
    let belongs_to_target: bool = sqlx::query_scalar(
        "select exists(select 1 from git_vault.snapshots
                        where snapshot_id = $1 and target_id = $2)",
    )
    .bind(candidate.snapshot_id)
    .bind(request.target_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    if !belongs_to_target {
        return Err(VaultError::InvalidDelivery {
            field: "retention_candidate_snapshot",
        });
    }
    let estimated_bytes =
        i64::try_from(candidate.estimated_bytes).map_err(|_| VaultError::InvalidDelivery {
            field: "retention_estimated_bytes",
        })?;
    let deletion_not_before = candidate
        .deletion_not_before_epoch_seconds
        .map(checked_epoch)
        .transpose()?;
    let pin_sources: Vec<_> = candidate
        .pin_sources
        .iter()
        .map(|source| source.as_str())
        .collect();
    sqlx::query(
        "insert into git_vault.retention_candidates
             (evaluation_id, snapshot_id, ordinal, classification, pin_sources,
              target_inactive, estimated_bytes, deletion_not_before)
         values ($1, $2, $3, $4, $5, $6, $7,
                 case when $8::bigint is null then null else to_timestamp($8) end)",
    )
    .bind(evaluation_id)
    .bind(candidate.snapshot_id)
    .bind(ordinal)
    .bind(candidate.classification.as_str())
    .bind(serde_json::json!(pin_sources))
    .bind(candidate.target_inactive)
    .bind(estimated_bytes)
    .bind(deletion_not_before)
    .execute(&mut **transaction)
    .await
    .map_err(storage_failure)?;
    append_audit(
        transaction,
        AuditEntry {
            target_id: request.target_id,
            snapshot_id: Some(candidate.snapshot_id),
            evaluation_id: Some(evaluation_id),
            plan_id: None,
            event_kind: "evaluation",
            reason: candidate.classification.as_str(),
            outcome: candidate.classification.audit_outcome(),
            correlation_id: request.correlation_id,
        },
    )
    .await
}
