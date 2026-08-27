//! Append-only wiki discovery evidence and idempotent sibling enrollment.

use ratatoskr_vault_core::error::VaultError;
use uuid::Uuid;

use crate::Database;

/// Closed terminal wiki discovery result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiDiscovery {
    /// The sibling repository exists and is eligible for ordinary mirror processing.
    Present,
    /// The provider confirmed absence after the parent was accessed successfully.
    Absent,
    /// Discovery was ambiguous or failed closed.
    Failed {
        /// Stable bounded failure class, never provider output.
        failure_class: String,
    },
}

/// Durable result of one wiki discovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiDiscoveryRecord {
    /// Append-only attempt identity.
    pub collector_run_id: Uuid,
    /// Idempotently enrolled sibling, only for [`WikiDiscovery::Present`].
    pub child_target_id: Option<Uuid>,
}

impl Database {
    /// Records one terminal discovery result and enrolls at most one sibling target.
    ///
    /// A present sibling receives a policy revision derived from the latest authorized parent
    /// revision. This is an execution projection, not a new policy source: wiki and auxiliary
    /// flags are cleared and the child always follows the Git mirror pipeline independently.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unknown/non-repository parent or unsafe failure class,
    /// and storage failure when the transaction cannot commit.
    pub async fn record_wiki_discovery(
        &self,
        parent_target_id: Uuid,
        discovery: WikiDiscovery,
    ) -> Result<WikiDiscoveryRecord, VaultError> {
        if matches!(&discovery, WikiDiscovery::Failed { failure_class } if failure_class.is_empty() || failure_class.len() > 64)
        {
            return Err(VaultError::InvalidDelivery {
                field: "wiki_failure_class",
            });
        }
        let mut tx = self.pool().begin().await.map_err(storage_failure)?;
        let parent: Option<(String, String)> = sqlx::query_as(
            "select provider, external_repository_id from git_vault.targets
             where target_id = $1 and target_kind = 'repository' for update",
        )
        .bind(parent_target_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_failure)?;
        let Some((provider, external_repository_id)) = parent else {
            return Err(VaultError::InvalidDelivery {
                field: "parent_target_id",
            });
        };

        let child_target_id = if discovery == WikiDiscovery::Present {
            let candidate = Uuid::now_v7();
            let child: Uuid = sqlx::query_scalar(
                "insert into git_vault.targets
                     (target_id, provider, external_repository_id, target_kind, parent_target_id,
                      status, created_at, updated_at)
                 values ($1, $2, $3, 'wiki', $4, 'requested', now(), now())
                 on conflict (parent_target_id) where target_kind = 'wiki'
                 do update set parent_target_id = excluded.parent_target_id
                 returning target_id",
            )
            .bind(candidate)
            .bind(provider)
            .bind(external_repository_id)
            .bind(parent_target_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_failure)?;

            sqlx::query(
                "insert into git_vault.desired_state_revisions
                     (revision_id, target_id, policy_revision, preservation_level, pinned,
                      include_wiki, include_releases, include_issues, offsite_required,
                      correlation_id, received_at)
                 select $2, $1, policy_revision, 'git_mirror', pinned,
                        false, false, false, offsite_required, correlation_id, now()
                 from git_vault.desired_state_revisions
                 where target_id = $3
                 order by policy_revision desc limit 1
                 on conflict (target_id, policy_revision) do nothing",
            )
            .bind(child)
            .bind(Uuid::now_v7())
            .bind(parent_target_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_failure)?;
            Some(child)
        } else {
            None
        };

        let collector_run_id = Uuid::now_v7();
        let (outcome, failure_class) = match discovery {
            WikiDiscovery::Present => ("complete", None),
            WikiDiscovery::Absent => ("absent", None),
            WikiDiscovery::Failed { failure_class } => ("failed", Some(failure_class)),
        };
        sqlx::query(
            "insert into git_vault.collector_runs
                 (collector_run_id, target_id, collector, outcome, child_target_id,
                  failure_class, ran_at)
             values ($1, $2, 'wiki', $3, $4, $5, now())",
        )
        .bind(collector_run_id)
        .bind(parent_target_id)
        .bind(outcome)
        .bind(child_target_id)
        .bind(failure_class)
        .execute(&mut *tx)
        .await
        .map_err(storage_failure)?;
        tx.commit().await.map_err(storage_failure)?;
        Ok(WikiDiscoveryRecord {
            collector_run_id,
            child_target_id,
        })
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err owns the database diagnostic before replacing it with the public taxonomy"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "wiki discovery persistence query failed");
    VaultError::StorageFailed
}
