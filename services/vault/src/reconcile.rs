//! The reconciliation cycle: deliveries enter through a narrow source, are validated once,
//! ingested, planned against observed state, and only executable work runs (designs D1, D5, D7).

use ratatoskr_vault_core::delivery::{IncomingDelivery, ValidatedDelivery, validate_delivery};
use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::planner::{PlanCounters, WorkItem, plan};
use ratatoskr_vault_core::target_state::TargetStatus;
use ratatoskr_vault_persistence::{Database, GoverningPolicy};
use uuid::Uuid;

/// Where undelivered messages come from (design D1).
///
/// The cycle depends on this seam: the policy lane hands it a `Vec` of the deliveries one policy
/// command produced, and fixtures implement it in tests.
pub trait DeliverySource {
    /// Hands over every message not delivered so far.
    fn fetch_undelivered(&mut self) -> Vec<IncomingDelivery>;
}

/// What one reconciliation cycle did, for callers and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleReport {
    /// Work items proposed across every converged target.
    pub proposed: usize,
    /// Items the executor could honestly perform.
    pub executable: usize,
}

/// Runs one full reconciliation cycle over the source's pending messages.
///
/// Each message is validated once and ingested atomically; every target a message names, replayed
/// or not, is then converged toward its governing revision by planning against observed state and executing
/// only what today's executor can honestly perform. Forward-looking runner work stays
/// planned-not-executable and shows up in the logged counters, never as success.
///
/// # Errors
///
/// [`VaultError::InvalidDelivery`] naming the rejected field when a delivery fails validation;
/// [`VaultError::IllegalTransition`] when the database guard refuses an executor move;
/// [`VaultError::StorageFailed`] for infrastructure failures, logged. A replayed
/// `(source, message_id)` pair is not ingested twice: the inbox refuses it, and the cycle still
/// converges the target it named, because the attempt that ingested it may have stopped before
/// converging.
pub async fn run_cycle(
    database: &Database,
    source: &mut impl DeliverySource,
) -> Result<CycleReport, VaultError> {
    let mut touched: Vec<Uuid> = Vec::new();
    let mut proposed: usize = 0;
    let mut executable: usize = 0;

    for message in source.fetch_undelivered() {
        let validated = validate_delivery(&message.delivery)?;
        match database
            .ingest_delivery(
                &message.provider,
                &message.external_repository_id,
                &message.source,
                message.message_id,
                &validated,
            )
            .await
        {
            Ok(target_id) => remember(&mut touched, target_id),
            // At-least-once transport redelivers and the inbox refused the pair, so nothing new
            // was written. The earlier attempt may have died before it converged the target, so
            // the target still joins the convergence below; planning against observed state
            // makes a repeat harmless.
            Err(VaultError::DuplicateDelivery) => {
                let target_id = database
                    .repository_target(&message.provider, &message.external_repository_id)
                    .await?;
                remember(&mut touched, target_id);
            }
            Err(err) => return Err(err),
        }
    }

    for target_id in &touched {
        let counters = converge_target(database, *target_id).await?;
        proposed += counters.proposed;
        executable += counters.executable;
    }

    Ok(CycleReport {
        proposed,
        executable,
    })
}

/// Adds a target to the set the cycle converges, once.
fn remember(touched: &mut Vec<Uuid>, target_id: Uuid) {
    if !touched.contains(&target_id) {
        touched.push(target_id);
    }
}

/// Moves one target toward its governing revision.
async fn converge_target(database: &Database, target_id: Uuid) -> Result<PlanCounters, VaultError> {
    let Some(policy) = database.governing_policy(target_id).await? else {
        return Ok(PlanCounters {
            proposed: 0,
            executable: 0,
        });
    };
    let desired = governed_record(&policy)?;
    let observed = database.observed_status(target_id).await?;

    let planned = plan(&desired, observed);
    tracing::info!(
        target = %target_id,
        proposed = planned.counters.proposed,
        executable = planned.counters.executable,
        divergences = planned.divergences.len(),
        "reconciliation plan built"
    );

    for item in &planned.items {
        match item {
            WorkItem::Exclude => {
                database
                    .apply_transition(target_id, TargetStatus::Excluded, &desired)
                    .await?;
            }
            WorkItem::Reactivate => {
                database
                    .apply_transition(target_id, TargetStatus::Requested, &desired)
                    .await?;
            }
            // Everything else runs silently today: enrollment already happened at first sight
            // inside the ingest transaction, evidence-only items leave nothing further to run,
            // and forward-looking runner work stays planned-not-executable (design D5).
            _ => {}
        }
    }

    Ok(planned.counters)
}

/// Rebuilds the validated record a governing revision arrived as, so the planner sees the same
/// shape the boundary validates.
fn governed_record(policy: &GoverningPolicy) -> Result<ValidatedDelivery, VaultError> {
    Ok(ValidatedDelivery {
        preservation_level: policy.preservation_level.clone(),
        pinned: Some(policy.pinned),
        include_wiki: Some(policy.include_wiki),
        include_releases: Some(policy.include_releases),
        include_issues: Some(policy.include_issues),
        offsite_required: Some(policy.offsite_required),
        correlation_id: policy.correlation_id.clone(),
        policy_revision: Some(u64::try_from(policy.policy_revision).map_err(|_| {
            VaultError::InvalidDelivery {
                field: "policy_revision",
            }
        })?),
    })
}
