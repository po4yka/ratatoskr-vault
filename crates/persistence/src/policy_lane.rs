//! The queries the policy lane needs: which targets a source governs, and the policy command
//! ledger that answers "what was the last applied policy version" and "did I already decide this
//! command" (XR-021 CONTRACTS.md section S09).

use ratatoskr_vault_core::error::VaultError;
use uuid::Uuid;

use crate::outbox_relay::ACKNOWLEDGEMENT_EVENT_TYPE;
use crate::{Database, classify_status_update_failure};

/// What Vault decided about one policy command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyCommandOutcome {
    /// The version moved the last applied version forward.
    Accepted,
    /// The version was refused.
    Rejected,
}

impl PolicyCommandOutcome {
    /// The ledger's stored token.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

/// One decision to record together with the acknowledgement that answers it.
#[derive(Debug, Clone, Copy)]
pub struct PolicyDecisionRecord<'a> {
    /// The command being answered; the ledger's primary key.
    pub command_id: Uuid,
    /// The policy version the command carried.
    pub policy_version: u64,
    /// Accepted or rejected.
    pub outcome: PolicyCommandOutcome,
    /// The stable rejection code when `outcome` is rejected.
    pub rejection_code: Option<&'a str>,
    /// The id of the acknowledgement event, which is also its outbox row id.
    pub acknowledgement_id: Uuid,
    /// The complete canonical acknowledgement envelope.
    pub acknowledgement: &'a serde_json::Value,
}

/// Whether a decision was written by this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedDecision {
    /// The command row and its acknowledgement were written.
    Recorded,
    /// The command had been decided before; nothing was written.
    AlreadyRecorded,
}

impl Database {
    /// The external ids of the repository targets whose governing revision, as it stood before
    /// policy version `version` of `source` was applied, came from `source` at a preservation
    /// level above `none`, sorted.
    ///
    /// The governing revision is the highest one, exactly as reconciliation reads it, so a newer
    /// revision from another source takes a target out of this list, and a target the source
    /// already withdrew in an earlier version is not listed again. The source's own revisions at
    /// `version` or above are ignored, so the answer is the same before the command's deliveries
    /// are ingested, while they are, and after they all are: a redelivered command recomputes the
    /// repositories it must withdraw even though its first attempt already wrote the withdrawal.
    ///
    /// # Errors
    ///
    /// [`VaultError::InvalidDelivery`] when `version` exceeds the storage range;
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn targets_governed_before(
        &self,
        source: &str,
        version: u64,
    ) -> Result<Vec<String>, VaultError> {
        let version = i64::try_from(version).map_err(|_| VaultError::InvalidDelivery {
            field: "policy_version",
        })?;
        sqlx::query_scalar(
            "select target.external_repository_id
             from git_vault.targets target
             join lateral (
                 select revision.source, revision.preservation_level
                 from git_vault.desired_state_revisions revision
                 where revision.target_id = target.target_id
                   and (revision.source is distinct from $1 or revision.policy_revision < $2)
                 order by revision.policy_revision desc
                 limit 1
             ) governing on true
             where target.target_kind = 'repository'
               and governing.source = $1
               and governing.preservation_level <> 'none'
             order by target.external_repository_id",
        )
        .bind(source)
        .bind(version)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))
    }

    /// The target of a repository the inbox already knows, for a replayed delivery that must
    /// still be converged.
    ///
    /// # Errors
    ///
    /// [`VaultError::InvalidDelivery`] naming `target_id` when the repository has no target;
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn repository_target(
        &self,
        provider: &str,
        external_repository_id: &str,
    ) -> Result<Uuid, VaultError> {
        sqlx::query_scalar(
            "select target_id from git_vault.targets
             where provider = $1 and external_repository_id = $2
               and target_kind = 'repository'",
        )
        .bind(provider)
        .bind(external_repository_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))?
        .ok_or(VaultError::InvalidDelivery { field: "target_id" })
    }

    /// The highest policy version Vault accepted, or zero before the first one.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn last_applied_policy_version(&self) -> Result<u64, VaultError> {
        let version: i64 = sqlx::query_scalar(
            "select coalesce(max(policy_version), 0)::bigint
             from git_vault.backup_policy_commands
             where outcome = 'accepted'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;
        u64::try_from(version).map_err(|_| VaultError::StorageFailed)
    }

    /// Whether a decision for `command_id` is already recorded.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn policy_command_recorded(&self, command_id: Uuid) -> Result<bool, VaultError> {
        sqlx::query_scalar(
            "select exists (select 1 from git_vault.backup_policy_commands where command_id = $1)",
        )
        .bind(command_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))
    }

    /// Records a decision and its acknowledgement in one transaction.
    ///
    /// A command decided before is found in the ledger first and reports
    /// [`RecordedDecision::AlreadyRecorded`] without writing anything. Otherwise the
    /// acknowledgement joins the outbox and the ledger row follows in the same transaction. The
    /// ledger forbids two accepted rows for one version, and that refusal rolls back the
    /// acknowledgement with it.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures and for a second accepted row for
    /// one version, logged.
    pub async fn record_policy_decision(
        &self,
        record: &PolicyDecisionRecord<'_>,
    ) -> Result<RecordedDecision, VaultError> {
        let version =
            i64::try_from(record.policy_version).map_err(|_| VaultError::InvalidDelivery {
                field: "policy_version",
            })?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| classify_status_update_failure(&error))?;

        let known: bool = sqlx::query_scalar(
            "select exists (select 1 from git_vault.backup_policy_commands where command_id = $1)",
        )
        .bind(record.command_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;
        if known {
            return Ok(RecordedDecision::AlreadyRecorded);
        }

        sqlx::query(
            "insert into git_vault.outbox
                 (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
             values ($1, $2, 'backup_policy', $3, $4, now())",
        )
        .bind(record.acknowledgement_id)
        .bind(ACKNOWLEDGEMENT_EVENT_TYPE)
        .bind(format!("backup_policy:{}", record.policy_version))
        .bind(record.acknowledgement)
        .execute(&mut *tx)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;

        sqlx::query(
            "insert into git_vault.backup_policy_commands
                 (command_id, policy_version, outcome, rejection_code, acknowledgement_id,
                  decided_at)
             values ($1, $2, $3, $4, $5, now())",
        )
        .bind(record.command_id)
        .bind(version)
        .bind(record.outcome.as_str())
        .bind(record.rejection_code)
        .bind(record.acknowledgement_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;

        tx.commit()
            .await
            .map_err(|error| classify_status_update_failure(&error))?;
        Ok(RecordedDecision::Recorded)
    }
}
