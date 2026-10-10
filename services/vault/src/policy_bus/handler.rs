//! What Vault decides about one policy command, as a function from bytes and the database to a
//! disposition. The consumer owns the broker and applies the disposition (S02 rule 7).

use ratatoskr_backup_contracts::VaultBackupPolicyApplyRequested;
use ratatoskr_event_envelope::{CommandEnvelope, CommandPayload as _};
use ratatoskr_identifiers::WireTimestamp;
use ratatoskr_vault_core::delivery::IncomingDelivery;
use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::policy_feed::{
    POLICY_SOURCE, PolicyDecision, acknowledgement_envelope, deliveries_for_policy,
};
use ratatoskr_vault_persistence::{Database, PolicyCommandOutcome, PolicyDecisionRecord};
use uuid::Uuid;

use crate::reconcile::{DeliverySource, run_cycle};

/// The only producer allowed to send this command (S01).
const PRODUCER: &str = "ratatoskr-github";

/// How the broker message is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Disposition {
    /// The outcome is durable.
    Ack,
    /// The input can never become valid.
    Term,
    /// A transient failure; redeliver after a short delay.
    Nak,
}

/// The deliveries one command produced, handed to the reconciliation cycle.
struct Batch(Vec<IncomingDelivery>);

impl DeliverySource for Batch {
    fn fetch_undelivered(&mut self) -> Vec<IncomingDelivery> {
        std::mem::take(&mut self.0)
    }
}

/// Decides one command. Never panics and never logs content: only a class token.
pub(super) async fn handle_command(database: &Database, payload: &[u8]) -> Disposition {
    let (command, request) = match decode(payload) {
        Ok(decoded) => decoded,
        Err(class) => {
            tracing::warn!(
                class,
                "a policy command can never be valid and was terminated"
            );
            return Disposition::Term;
        }
    };
    match decide(database, &command, &request).await {
        Ok(()) => Disposition::Ack,
        // A delivery the cycle cannot ingest at all (a version beyond the storage range) will not
        // improve on redelivery.
        Err(VaultError::InvalidDelivery { field }) => {
            tracing::warn!(
                field,
                "a policy command carries an unusable value and was terminated"
            );
            Disposition::Term
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "a policy command failed transiently and will be redelivered"
            );
            Disposition::Nak
        }
    }
}

/// The checks that need no state: the envelope, its type, its producer, its scope and its payload.
fn decode(
    payload: &[u8],
) -> Result<(CommandEnvelope, VaultBackupPolicyApplyRequested), &'static str> {
    let command = CommandEnvelope::from_json(payload).map_err(|_| "undecodable_envelope")?;
    if command.command_type.to_wire() != VaultBackupPolicyApplyRequested::COMMAND_TYPE {
        return Err("unknown_command_type");
    }
    if command.producer.as_str() != PRODUCER {
        return Err("wrong_producer");
    }
    if command.tenant_id.is_some() {
        return Err("unexpected_tenant");
    }
    let request = command
        .payload_as::<VaultBackupPolicyApplyRequested>()
        .map_err(|_| "undecodable_payload")?;
    Ok((command, request))
}

/// Applies an acceptable version, then records the decision with its acknowledgement.
///
/// A crash between the two leaves no ledger row, so the redelivery decides again from the same
/// state: the repositories to withdraw are read as they stood before the command, so the
/// deliveries are the same ones with the same message ids (the inbox absorbs the ones already
/// ingested), the cycle converges every repository they name whether or not its ingest was a
/// replay, and only then does the last applied version move.
async fn decide(
    database: &Database,
    command: &CommandEnvelope,
    request: &VaultBackupPolicyApplyRequested,
) -> Result<(), VaultError> {
    let command_id = command.command_id.0;
    if database.policy_command_recorded(command_id).await? {
        return Ok(());
    }
    let last_applied = database.last_applied_policy_version().await?;
    let decision = PolicyDecision::for_version(request.policy.policy_version, last_applied);
    if matches!(decision, PolicyDecision::Accepted { .. }) {
        apply(database, command_id, request, last_applied).await?;
    }
    record(database, command, &decision).await
}

/// Runs the cycle over the deliveries the document produces.
async fn apply(
    database: &Database,
    command_id: Uuid,
    request: &VaultBackupPolicyApplyRequested,
    last_applied: u64,
) -> Result<(), VaultError> {
    let governed = database
        .targets_governed_before(POLICY_SOURCE, request.policy.policy_version)
        .await?;
    let deliveries = deliveries_for_policy(command_id, &request.policy, last_applied, &governed)
        .map_err(|_| VaultError::InvalidDelivery {
            field: "policy_version",
        })?;
    run_cycle(database, &mut Batch(deliveries))
        .await
        .map(|_| ())
}

/// Writes the ledger row and the acknowledgement envelope in one transaction.
async fn record(
    database: &Database,
    command: &CommandEnvelope,
    decision: &PolicyDecision,
) -> Result<(), VaultError> {
    let acknowledgement_id = Uuid::now_v7();
    let envelope =
        acknowledgement_envelope(command, acknowledgement_id, WireTimestamp::now(), decision)
            .map_err(|_| VaultError::StorageFailed)?;
    let body = serde_json::to_value(&envelope).map_err(|_| VaultError::StorageFailed)?;
    let (policy_version, outcome, rejection_code) = match *decision {
        PolicyDecision::Accepted { policy_version, .. } => {
            (policy_version, PolicyCommandOutcome::Accepted, None)
        }
        PolicyDecision::Rejected {
            policy_version,
            code,
            ..
        } => (
            policy_version,
            PolicyCommandOutcome::Rejected,
            Some(code.as_str()),
        ),
    };
    database
        .record_policy_decision(&PolicyDecisionRecord {
            command_id: command.command_id.0,
            policy_version,
            outcome,
            rejection_code,
            acknowledgement_id,
            acknowledgement: &body,
        })
        .await
        .map(|_| ())
}
