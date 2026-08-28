//! Observable contracts for deterministic, pin-aware snapshot retention.

use std::collections::BTreeSet;

use ratatoskr_vault_core::retention::{
    DecisionReason, PinSource, PressureSelection, PressureTier, QuotaPressureDecision,
    RetentionPolicy, RetentionSnapshot, SnapshotDecision, evaluate_quota_pressure,
    evaluate_retention,
};
use uuid::Uuid;

fn snapshot(mirror: u128, snapshot: u128, created_at: u64) -> RetentionSnapshot {
    RetentionSnapshot {
        snapshot_id: Uuid::from_u128(snapshot),
        mirror_id: Uuid::from_u128(mirror),
        created_at_epoch_seconds: created_at,
        restorable: true,
        active_pins: BTreeSet::new(),
        target_inactive: false,
        deletion_not_before_epoch_seconds: None,
        estimated_bytes: 4,
    }
}

#[test]
fn selection_is_deterministic_per_mirror() {
    let policy = RetentionPolicy {
        keep_last_restorable: 1,
        minimum_age_seconds: 100,
        grace_seconds: 100,
    };
    let snapshots = vec![
        snapshot(2, 22, 300),
        snapshot(1, 13, 300),
        snapshot(1, 11, 100),
        snapshot(2, 21, 100),
        snapshot(1, 12, 200),
        RetentionSnapshot {
            restorable: false,
            ..snapshot(1, 14, 950)
        },
    ];
    let mut reversed = snapshots.clone();
    reversed.reverse();

    let forward = evaluate_retention(&snapshots, policy, 1_000);
    let backward = evaluate_retention(&reversed, policy, 1_000);

    assert_eq!(
        forward, backward,
        "classification and ordering must not depend on input order"
    );
    assert_eq!(
        forward,
        vec![
            decision(11, DecisionReason::EligibleOrdinary),
            decision(12, DecisionReason::EligibleOrdinary),
            decision(13, DecisionReason::ProtectedKeepLastRestorable),
            decision(14, DecisionReason::ProtectedAgeFloor),
            decision(21, DecisionReason::EligibleOrdinary),
            decision(22, DecisionReason::ProtectedKeepLastRestorable),
        ]
    );
}

fn decision(snapshot: u128, reason: DecisionReason) -> SnapshotDecision {
    SnapshotDecision {
        snapshot_id: Uuid::from_u128(snapshot),
        reason,
    }
}

#[test]
fn pins_and_last_restorable_snapshot_are_protected() {
    let policy = RetentionPolicy {
        keep_last_restorable: 1,
        minimum_age_seconds: 0,
        grace_seconds: 100,
    };
    let snapshots = vec![
        RetentionSnapshot {
            restorable: false,
            active_pins: BTreeSet::from([PinSource::Operator]),
            ..snapshot(1, 11, 100)
        },
        snapshot(1, 12, 200),
        RetentionSnapshot {
            restorable: false,
            active_pins: BTreeSet::from([PinSource::User]),
            ..snapshot(2, 21, 100)
        },
    ];

    let decisions = evaluate_retention(&snapshots, policy, 1_000);

    assert_eq!(
        decisions,
        vec![
            decision(11, DecisionReason::ProtectedPinned),
            decision(12, DecisionReason::ProtectedKeepLastRestorable),
            decision(21, DecisionReason::ProtectedPinned),
        ]
    );
}

#[test]
fn quota_pressure_uses_fixed_tiers_then_refuses() {
    let policy = RetentionPolicy {
        keep_last_restorable: 1,
        minimum_age_seconds: 0,
        grace_seconds: 100,
    };
    let snapshots = vec![
        RetentionSnapshot {
            restorable: false,
            deletion_not_before_epoch_seconds: Some(500),
            ..snapshot(1, 11, 100)
        },
        snapshot(1, 12, 200),
        RetentionSnapshot {
            restorable: false,
            target_inactive: true,
            deletion_not_before_epoch_seconds: Some(500),
            ..snapshot(2, 21, 100)
        },
        snapshot(2, 22, 200),
        RetentionSnapshot {
            restorable: false,
            deletion_not_before_epoch_seconds: Some(1_100),
            estimated_bytes: 100,
            ..snapshot(3, 31, 50)
        },
        RetentionSnapshot {
            restorable: false,
            active_pins: BTreeSet::from([PinSource::Operator]),
            deletion_not_before_epoch_seconds: Some(500),
            estimated_bytes: 100,
            ..snapshot(4, 41, 25)
        },
    ];

    let decision = evaluate_quota_pressure(&snapshots, policy, 1_000, 10);

    assert_eq!(
        decision,
        QuotaPressureDecision {
            selections: vec![
                PressureSelection {
                    snapshot_id: Uuid::from_u128(11),
                    tier: PressureTier::Ordinary,
                    estimated_bytes: 4,
                },
                PressureSelection {
                    snapshot_id: Uuid::from_u128(21),
                    tier: PressureTier::InactiveTarget,
                    estimated_bytes: 4,
                },
            ],
            freed_bytes: 8,
            required_bytes: 10,
            allocation_refused: true,
        }
    );
}
