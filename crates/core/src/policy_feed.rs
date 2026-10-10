//! The pure adapter from GitHub's desired backup policy to Vault's desired-state deliveries.
//!
//! No I/O lives here: the last applied version and the repositories a previous document governed
//! arrive as arguments, so the whole-catalog semantics are decided in one place and tested with
//! plain values (XR-021 CONTRACTS.md section S09).

use ratatoskr_backup_contracts::{
    DesiredBackupPolicy, PolicyAcknowledged, PolicyOutcome, PolicyRejectionCode,
    PolicyRejectionReason,
};
use ratatoskr_event_envelope::{
    CommandEnvelope, EnvelopeSchemaVersion, EventEnvelope, EventPayload as _, ProducerName,
};
use ratatoskr_identifiers::{EntityRef, EventId, Extensions, WireTimestamp};
use uuid::Uuid;

use crate::delivery::{DesiredStateDelivery, IncomingDelivery};

/// The inbox source label of the policy lane.
pub const POLICY_SOURCE: &str = "github-policy";

/// The provider that owns every repository the policy names.
const PROVIDER: &str = "github";

/// A policy document the adapter refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyFeedError {
    /// The candidate version does not strictly exceed the last applied one.
    #[error("policy version {candidate} does not exceed the last applied version {last_applied}")]
    PolicyVersionNotMonotonic {
        /// The version the document carries.
        candidate: u64,
        /// The last version Vault applied.
        last_applied: u64,
    },
}

/// Maps one policy document to the deliveries that make Vault converge on it.
///
/// # Errors
///
/// [`PolicyFeedError::PolicyVersionNotMonotonic`] when `policy.policy_version` does not exceed
/// `last_applied_version`.
pub fn deliveries_for_policy(
    command_id: Uuid,
    policy: &DesiredBackupPolicy,
    last_applied_version: u64,
    previously_governed: &[String],
) -> Result<Vec<IncomingDelivery>, PolicyFeedError> {
    if policy.policy_version <= last_applied_version {
        return Err(PolicyFeedError::PolicyVersionNotMonotonic {
            candidate: policy.policy_version,
            last_applied: last_applied_version,
        });
    }

    let wanted: Vec<String> = policy
        .repositories
        .iter()
        .map(|entry| entry.repository_ref.to_wire())
        .collect();
    let dropped = previously_governed
        .iter()
        .filter(|reference| !wanted.contains(reference));

    let mirrored = wanted
        .iter()
        .map(|reference| delivery(command_id, policy.policy_version, reference, MIRRORED));
    let withdrawn =
        dropped.map(|reference| delivery(command_id, policy.policy_version, reference, WITHDRAWN));
    Ok(mirrored.chain(withdrawn).collect())
}

/// The preservation level of a repository the document names.
const MIRRORED: &str = "git_mirror";

/// The preservation level of a previously governed repository the document no longer names.
const WITHDRAWN: &str = "none";

/// One delivery for `repository_ref` at `preservation_level`.
fn delivery(
    command_id: Uuid,
    policy_version: u64,
    repository_ref: &str,
    preservation_level: &str,
) -> IncomingDelivery {
    IncomingDelivery {
        provider: PROVIDER.to_owned(),
        external_repository_id: repository_ref.to_owned(),
        source: POLICY_SOURCE.to_owned(),
        message_id: Uuid::new_v5(&command_id, repository_ref.as_bytes()),
        delivery: DesiredStateDelivery {
            preservation_level: preservation_level.to_owned(),
            pinned: None,
            include_wiki: None,
            include_releases: None,
            include_issues: None,
            offsite_required: None,
            correlation_id: format!("backup_policy:{policy_version}"),
            policy_revision: Some(policy_version),
        },
    }
}

/// What Vault decided about one policy version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    /// The version moved the last applied version forward.
    Accepted {
        /// The version that was applied.
        policy_version: u64,
        /// The last applied version before this decision.
        previous_applied: u64,
    },
    /// The version was refused.
    Rejected {
        /// The version that was refused.
        policy_version: u64,
        /// The last applied version, which is unchanged.
        last_applied: u64,
        /// Why it was refused.
        code: PolicyRejectionCode,
    },
}

impl PolicyDecision {
    /// The decision for a candidate version against the last applied one.
    #[must_use]
    pub fn for_version(policy_version: u64, last_applied: u64) -> Self {
        if policy_version > last_applied {
            Self::Accepted {
                policy_version,
                previous_applied: last_applied,
            }
        } else {
            Self::Rejected {
                policy_version,
                last_applied,
                code: PolicyRejectionCode::PolicyVersionNotMonotonic,
            }
        }
    }

    /// The contract payload that states this decision.
    fn payload(&self) -> PolicyAcknowledged {
        match *self {
            Self::Accepted {
                policy_version,
                previous_applied,
            } => PolicyAcknowledged {
                acknowledged_policy_version: policy_version,
                outcome: PolicyOutcome::Accepted,
                reasons: Vec::new(),
                last_applied_policy_version: previous_applied,
                extensions: Extensions::new(),
            },
            Self::Rejected {
                policy_version,
                last_applied,
                code,
            } => PolicyAcknowledged {
                acknowledged_policy_version: policy_version,
                outcome: PolicyOutcome::Rejected,
                reasons: vec![PolicyRejectionReason {
                    code,
                    repository_ref: None,
                }],
                last_applied_policy_version: last_applied,
                extensions: Extensions::new(),
            },
        }
    }
}

/// A complete acknowledgement could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the acknowledgement envelope could not be built: {0}")]
pub struct AcknowledgementError(pub String);

/// Builds the canonical acknowledgement event answering `command`.
///
/// # Errors
///
/// [`AcknowledgementError`] when an identifier or the payload is not representable.
pub fn acknowledgement_envelope(
    command: &CommandEnvelope,
    event_id: Uuid,
    occurred_at: WireTimestamp,
    decision: &PolicyDecision,
) -> Result<EventEnvelope, AcknowledgementError> {
    let (PolicyDecision::Accepted { policy_version, .. }
    | PolicyDecision::Rejected { policy_version, .. }) = *decision;
    let aggregate_id = EntityRef::parse(&format!("backup_policy:{policy_version}"))
        .map_err(|error| AcknowledgementError(error.to_string()))?;
    let causation_id = EntityRef::parse(&format!("command:{}", command.command_id))
        .map_err(|error| AcknowledgementError(error.to_string()))?;
    let producer = ProducerName::parse("ratatoskr-vault")
        .map_err(|error| AcknowledgementError(error.to_string()))?;
    let payload = decision.payload();
    payload
        .validate()
        .map_err(|error| AcknowledgementError(error.to_string()))?;
    let mut envelope = EventEnvelope {
        event_id: EventId(event_id),
        event_type: PolicyAcknowledged::event_type(),
        occurred_at,
        producer,
        aggregate_id,
        correlation_id: command.correlation_id.clone(),
        causation_id: Some(causation_id),
        tenant_id: None,
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::new(),
    };
    envelope
        .set_payload(&payload)
        .map_err(|error| AcknowledgementError(error.to_string()))?;
    Ok(envelope)
}
