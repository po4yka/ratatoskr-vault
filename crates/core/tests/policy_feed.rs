//! The policy adapter: one `DesiredBackupPolicy` becomes one delivery per repository entry, a
//! `none` delivery for every previously governed repository the document dropped, and a refusal
//! for a version that does not move forward. The acknowledgement builder answers a command with a
//! complete canonical envelope (XR-021 CONTRACTS.md section S09).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_backup_contracts::{
    DesiredBackupPolicy, PolicyAcknowledged, PolicyOutcome, PolicyRejectionCode,
};
use ratatoskr_event_envelope::{CommandEnvelope, EventEnvelope};
use ratatoskr_identifiers::WireTimestamp;
use ratatoskr_vault_core::delivery::IncomingDelivery;
use ratatoskr_vault_core::policy_feed::{
    POLICY_SOURCE, PolicyDecision, PolicyFeedError, acknowledgement_envelope, deliveries_for_policy,
};
use serde_json::json;
use uuid::Uuid;

const REPO_A: &str = "repository:018f0000-0000-7000-8000-000000000501";
const REPO_B: &str = "repository:018f0000-0000-7000-8000-000000000502";
const REPO_C: &str = "repository:018f0000-0000-7000-8000-000000000503";
const COMMAND_ID: &str = "018f0000-0000-7000-8000-0000000000c1";

fn command_id() -> Uuid {
    Uuid::parse_str(COMMAND_ID).expect("a fixed command id")
}

/// A policy document naming exactly `repositories` at `version`.
fn policy(version: u64, repositories: &[&str]) -> DesiredBackupPolicy {
    let entries: Vec<serde_json::Value> = repositories
        .iter()
        .map(|reference| {
            json!({
                "repository_ref": reference,
                "mirror_cadence": "daily",
                "priority_hint": "standard",
            })
        })
        .collect();
    serde_json::from_value(json!({
        "policy_version": version,
        "producing_service": "ratatoskr-github",
        "produced_at": "2026-08-20T09:00:00Z",
        "repositories": entries,
    }))
    .expect("a valid policy document")
}

fn governed(references: &[&str]) -> Vec<String> {
    references
        .iter()
        .map(|reference| (*reference).to_owned())
        .collect()
}

fn delivery_for<'a>(deliveries: &'a [IncomingDelivery], reference: &str) -> &'a IncomingDelivery {
    let found = deliveries
        .iter()
        .find(|delivery| delivery.external_repository_id == reference);
    assert!(found.is_some(), "no delivery for {reference}");
    found.expect("asserted just above")
}

/// Two entries and previously governed {A, B, C} with B absent: A and C are mirrored, B is
/// withdrawn, every delivery carries the contract identity, and a version that does not exceed
/// the last applied one is refused.
#[test]
fn policy_document_becomes_one_delivery_per_repository_and_none_for_dropped_targets() {
    let document = policy(7, &[REPO_A, REPO_C]);

    let deliveries = deliveries_for_policy(
        command_id(),
        &document,
        6,
        &governed(&[REPO_A, REPO_B, REPO_C]),
    )
    .expect("version 7 follows version 6");

    assert_eq!(deliveries.len(), 3, "A and C are mirrored, B is withdrawn");
    for reference in [REPO_A, REPO_C] {
        let delivery = delivery_for(&deliveries, reference);
        assert_eq!(delivery.delivery.preservation_level, "git_mirror");
    }
    assert_eq!(
        delivery_for(&deliveries, REPO_B)
            .delivery
            .preservation_level,
        "none"
    );
    for delivery in &deliveries {
        assert_eq!(delivery.provider, "github");
        assert_eq!(delivery.source, POLICY_SOURCE);
        assert_eq!(delivery.delivery.policy_revision, Some(7));
        assert_eq!(delivery.delivery.correlation_id, "backup_policy:7");
        assert_eq!(
            delivery.message_id,
            Uuid::new_v5(&command_id(), delivery.external_repository_id.as_bytes()),
            "the message id is UUIDv5(command_id, repository_ref)"
        );
    }

    assert_eq!(
        deliveries_for_policy(command_id(), &document, 7, &governed(&[REPO_A])),
        Err(PolicyFeedError::PolicyVersionNotMonotonic {
            candidate: 7,
            last_applied: 7
        })
    );
    assert!(matches!(
        deliveries_for_policy(command_id(), &document, 9, &[]),
        Err(PolicyFeedError::PolicyVersionNotMonotonic { .. })
    ));
}

/// The same command always produces the same deliveries, so a redelivery is absorbed by the
/// inbox; a different command produces different message ids.
#[test]
fn a_redelivered_command_reproduces_the_same_message_ids() {
    let document = policy(3, &[REPO_A]);
    let first = deliveries_for_policy(command_id(), &document, 2, &[]).expect("monotonic");
    let again = deliveries_for_policy(command_id(), &document, 2, &[]).expect("monotonic");
    let other = deliveries_for_policy(
        Uuid::parse_str("018f0000-0000-7000-8000-0000000000c2").expect("a command id"),
        &document,
        2,
        &[],
    )
    .expect("monotonic");

    assert_eq!(first.len(), 1);
    assert_eq!(first, again);
    assert_ne!(first[0].message_id, other[0].message_id);
}

/// An empty document withdraws everything previously governed and nothing else.
#[test]
fn an_empty_document_withdraws_every_governed_repository() {
    let document = policy(4, &[]);
    let deliveries =
        deliveries_for_policy(command_id(), &document, 3, &governed(&[REPO_A, REPO_B]))
            .expect("monotonic");

    assert_eq!(deliveries.len(), 2);
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery.delivery.preservation_level == "none")
    );
}

/// The command the acknowledgements answer.
fn command(version: u64) -> CommandEnvelope {
    CommandEnvelope::from_json(
        json!({
            "command_id": COMMAND_ID,
            "command_type": "vault.backup_policy.apply_requested.v1",
            "issued_at": "2026-08-20T09:00:00Z",
            "producer": "ratatoskr-github",
            "aggregate_id": format!("backup_policy:{version}"),
            "correlation_id": format!("backup_policy:{version}"),
            "schema_version": 1,
            "payload": {},
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a valid command envelope")
}

fn event_id() -> Uuid {
    Uuid::parse_str("018f0000-0000-7000-8000-0000000000e1").expect("a fixed event id")
}

fn occurred_at() -> WireTimestamp {
    WireTimestamp::parse("2026-08-20T09:00:05Z").expect("a canonical timestamp")
}

/// An accepted and a rejected acknowledgement are complete envelopes: identity, producer,
/// aggregate, correlation copied from the command, causation naming the command, no tenant, and
/// a payload that validates and survives a canonical JSON round trip.
#[test]
fn acknowledgements_are_complete_envelopes_for_both_outcomes() {
    let accepted_decision = PolicyDecision::for_version(7, 6);
    let rejected_decision = PolicyDecision::for_version(5, 9);
    assert_eq!(
        rejected_decision,
        PolicyDecision::Rejected {
            policy_version: 5,
            last_applied: 9,
            code: PolicyRejectionCode::PolicyVersionNotMonotonic
        }
    );

    let accepted =
        acknowledgement_envelope(&command(7), event_id(), occurred_at(), &accepted_decision)
            .expect("an accepted acknowledgement");
    assert_eq!(accepted.event_id.0, event_id());
    assert_eq!(
        accepted.event_type.to_wire(),
        "vault.backup_policy.acknowledged.v1"
    );
    assert_eq!(accepted.producer.as_str(), "ratatoskr-vault");
    assert_eq!(accepted.aggregate_id.to_wire(), "backup_policy:7");
    assert_eq!(accepted.correlation_id.to_wire(), "backup_policy:7");
    assert_eq!(
        accepted.causation_id.as_ref().map(ToString::to_string),
        Some(format!("command:{COMMAND_ID}"))
    );
    assert!(accepted.tenant_id.is_none(), "the policy is catalog-wide");
    let payload: PolicyAcknowledged = accepted.payload_as().expect("a valid payload");
    assert_eq!(payload.outcome, PolicyOutcome::Accepted);
    assert_eq!(payload.acknowledged_policy_version, 7);
    assert_eq!(payload.last_applied_policy_version, 6);
    assert!(payload.reasons.is_empty());

    let rejected =
        acknowledgement_envelope(&command(5), event_id(), occurred_at(), &rejected_decision)
            .expect("a rejected acknowledgement");
    assert_eq!(rejected.aggregate_id.to_wire(), "backup_policy:5");
    let payload: PolicyAcknowledged = rejected.payload_as().expect("a valid payload");
    assert_eq!(payload.outcome, PolicyOutcome::Rejected);
    assert_eq!(payload.acknowledged_policy_version, 5);
    assert_eq!(payload.last_applied_policy_version, 9);
    assert_eq!(payload.reasons.len(), 1);
    assert_eq!(
        payload.reasons[0].code,
        PolicyRejectionCode::PolicyVersionNotMonotonic
    );

    let canonical = rejected.to_canonical_json().expect("canonical JSON");
    assert_eq!(
        EventEnvelope::from_json(canonical.as_bytes()).expect("the canonical form parses"),
        rejected
    );
}
