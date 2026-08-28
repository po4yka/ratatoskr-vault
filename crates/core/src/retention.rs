//! Deterministic retention-policy vocabulary, independent of storage and orchestration.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

/// One validated retention policy evaluated for a mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Number of newest restorable snapshots retained per mirror.
    pub keep_last_restorable: usize,
    /// Minimum age before a snapshot can become eligible.
    pub minimum_age_seconds: u64,
    /// Fixed delay between durable deletion intent and the first physical effect.
    pub grace_seconds: u64,
}

/// Vault's local default until an explicit target policy is selected by configuration.
pub const DEFAULT_RETENTION_POLICY: RetentionPolicy = RetentionPolicy {
    keep_last_restorable: 3,
    minimum_age_seconds: 2_592_000,
    grace_seconds: 2_592_000,
};

/// One normalized snapshot considered by retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSnapshot {
    /// Stable snapshot identity.
    pub snapshot_id: Uuid,
    /// Mirror whose keep-N window owns this snapshot.
    pub mirror_id: Uuid,
    /// Snapshot creation time as UTC Unix seconds.
    pub created_at_epoch_seconds: u64,
    /// Whether verification and restore evidence make this snapshot restorable.
    pub restorable: bool,
    /// Active durable pin sources protecting this snapshot.
    pub active_pins: BTreeSet<PinSource>,
    /// Whether the owning target is inactive and grace-complete.
    pub target_inactive: bool,
    /// Fixed snapshot deletion deadline, when a tombstoned intent exists.
    pub deletion_not_before_epoch_seconds: Option<u64>,
    /// Finite bytes this snapshot can release under pressure.
    pub estimated_bytes: u64,
}

/// A durable authority that can pin one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinSource {
    /// An explicit operator action.
    Operator,
    /// An upstream user-directed action.
    User,
}

impl PinSource {
    /// Canonical database vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::User => "user",
        }
    }
}

/// One retention classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotDecision {
    /// Snapshot being classified.
    pub snapshot_id: Uuid,
    /// Why the snapshot is protected or eligible.
    pub reason: DecisionReason,
}

/// Closed reason vocabulary emitted by the pure policy engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReason {
    /// The snapshot is old enough and outside keep-N protection.
    EligibleOrdinary,
    /// The snapshot is younger than the policy floor.
    ProtectedAgeFloor,
    /// The snapshot is one of the newest restorable snapshots retained for its mirror.
    ProtectedKeepLastRestorable,
    /// At least one durable pin remains active.
    ProtectedPinned,
}

/// Escalation tier used by quota-pressure selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureTier {
    /// An ordinary policy-due snapshot on an active target.
    Ordinary,
    /// A snapshot belonging to a grace-complete inactive target.
    InactiveTarget,
}

/// One safely selectable pressure candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureSelection {
    /// Snapshot whose physical bytes may be released.
    pub snapshot_id: Uuid,
    /// Fixed escalation tier.
    pub tier: PressureTier,
    /// Estimated releasable bytes.
    pub estimated_bytes: u64,
}

/// Complete quota-pressure outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPressureDecision {
    /// Safely selected candidates in escalation order.
    pub selections: Vec<PressureSelection>,
    /// Saturating sum of selected estimates.
    pub freed_bytes: u64,
    /// Requested capacity.
    pub required_bytes: u64,
    /// Whether safe candidates cannot meet the request.
    pub allocation_refused: bool,
}

/// Classifies snapshots for one scheduled evaluation.
///
#[must_use]
pub fn evaluate_retention(
    snapshots: &[RetentionSnapshot],
    policy: RetentionPolicy,
    evaluated_at_epoch_seconds: u64,
) -> Vec<SnapshotDecision> {
    let mut restorable_by_mirror: BTreeMap<Uuid, Vec<&RetentionSnapshot>> = BTreeMap::new();
    for snapshot in snapshots.iter().filter(|snapshot| snapshot.restorable) {
        restorable_by_mirror
            .entry(snapshot.mirror_id)
            .or_default()
            .push(snapshot);
    }
    let mut keep_last = BTreeSet::new();
    for restorable in restorable_by_mirror.values_mut() {
        restorable.sort_by(|left, right| {
            right
                .created_at_epoch_seconds
                .cmp(&left.created_at_epoch_seconds)
                .then_with(|| right.snapshot_id.cmp(&left.snapshot_id))
        });
        keep_last.extend(
            restorable
                .iter()
                .take(policy.keep_last_restorable.max(1))
                .map(|snapshot| snapshot.snapshot_id),
        );
    }

    let mut ordered: Vec<_> = snapshots.iter().collect();
    ordered.sort_by_key(|snapshot| {
        (
            snapshot.mirror_id,
            snapshot.created_at_epoch_seconds,
            snapshot.snapshot_id,
        )
    });
    ordered
        .into_iter()
        .map(|snapshot| SnapshotDecision {
            snapshot_id: snapshot.snapshot_id,
            reason: if !snapshot.active_pins.is_empty() {
                DecisionReason::ProtectedPinned
            } else if evaluated_at_epoch_seconds.saturating_sub(snapshot.created_at_epoch_seconds)
                < policy.minimum_age_seconds
            {
                DecisionReason::ProtectedAgeFloor
            } else if keep_last.contains(&snapshot.snapshot_id) {
                DecisionReason::ProtectedKeepLastRestorable
            } else {
                DecisionReason::EligibleOrdinary
            },
        })
        .collect()
}

/// Selects only already tombstoned, grace-complete snapshots for quota pressure.
#[must_use]
pub fn evaluate_quota_pressure(
    snapshots: &[RetentionSnapshot],
    policy: RetentionPolicy,
    evaluated_at_epoch_seconds: u64,
    required_bytes: u64,
) -> QuotaPressureDecision {
    let decisions: BTreeMap<_, _> =
        evaluate_retention(snapshots, policy, evaluated_at_epoch_seconds)
            .into_iter()
            .map(|decision| (decision.snapshot_id, decision.reason))
            .collect();
    let mut candidates: Vec<_> = snapshots
        .iter()
        .filter(|snapshot| {
            decisions.get(&snapshot.snapshot_id) == Some(&DecisionReason::EligibleOrdinary)
                && snapshot
                    .deletion_not_before_epoch_seconds
                    .is_some_and(|deadline| deadline <= evaluated_at_epoch_seconds)
        })
        .collect();
    candidates.sort_by_key(|snapshot| {
        (
            snapshot.target_inactive,
            snapshot.created_at_epoch_seconds,
            snapshot.mirror_id,
            snapshot.snapshot_id,
        )
    });

    let mut selections = Vec::new();
    let mut freed_bytes = 0_u64;
    for snapshot in candidates {
        if freed_bytes >= required_bytes {
            break;
        }
        selections.push(PressureSelection {
            snapshot_id: snapshot.snapshot_id,
            tier: if snapshot.target_inactive {
                PressureTier::InactiveTarget
            } else {
                PressureTier::Ordinary
            },
            estimated_bytes: snapshot.estimated_bytes,
        });
        freed_bytes = freed_bytes.saturating_add(snapshot.estimated_bytes);
    }

    QuotaPressureDecision {
        selections,
        freed_bytes,
        required_bytes,
        allocation_refused: freed_bytes < required_bytes,
    }
}
