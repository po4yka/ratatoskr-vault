//! The closed vocabulary of durable target statuses.
//!
//! Every status a Vault backup target can hold is a variant of [`TargetStatus`]. The
//! vocabulary is closed: the database CHECK constraint and this enum must agree on the exact
//! `snake_case` names, and every legal move between statuses is defined by the transition table
//! in this module rather than inferred from nullable columns.

/// A durable status of a backup target in the Vault state machine.
///
/// Persisted statuses carry the canonical [`TargetStatus::as_str`] name, which must match the database CHECK constraint exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetStatus {
    /// The desired state has been accepted; no acquisition work has started yet.
    Requested,
    /// The initial mirror clone from the source host is running.
    Cloning,
    /// The mirror exists and passed integrity checks; refresh cycles may begin.
    Ready,
    /// An incremental fetch from the source host is running.
    Fetching,
    /// An immutable snapshot artifact is being built from the mirror.
    Snapshotting,
    /// A snapshot artifact is being verified against its manifest.
    Verifying,
    /// The latest snapshot is verified; every required policy component holds.
    Healthy,
    /// The latest refresh or verification failed while an earlier verified snapshot survives.
    Degraded,
    /// Refreshes are suspended by request; all stored state is retained.
    Paused,
    /// Desired state was withdrawn; the target is retained for retention evaluation only.
    Excluded,
    /// Retention decided deletion and the staged deletion workflow owns the target.
    Deleting,
}

impl TargetStatus {
    /// Every status of the closed vocabulary, in canonical order.
    pub const ALL: [Self; 11] = [
        Self::Requested,
        Self::Cloning,
        Self::Ready,
        Self::Fetching,
        Self::Snapshotting,
        Self::Verifying,
        Self::Healthy,
        Self::Degraded,
        Self::Paused,
        Self::Excluded,
        Self::Deleting,
    ];

    /// The canonical `snake_case` name of this status as persisted in the schema.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Cloning => "cloning",
            Self::Ready => "ready",
            Self::Fetching => "fetching",
            Self::Snapshotting => "snapshotting",
            Self::Verifying => "verifying",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Paused => "paused",
            Self::Excluded => "excluded",
            Self::Deleting => "deleting",
        }
    }
}

/// A directed move between two durable target statuses.
///
/// Only the moves enumerated by the design matrix are legal; [`Transition::is_legal`] rejects every other pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    /// The status the target moves out of.
    pub from: TargetStatus,
    /// The status the target moves into.
    pub to: TargetStatus,
}

impl Transition {
    /// Every legal move of the design matrix, as `(from, to)` pairs.
    const TRANSITIONS: [(TargetStatus, TargetStatus); 37] = [
        (TargetStatus::Requested, TargetStatus::Cloning),
        (TargetStatus::Requested, TargetStatus::Excluded),
        (TargetStatus::Requested, TargetStatus::Deleting),
        (TargetStatus::Cloning, TargetStatus::Ready),
        (TargetStatus::Cloning, TargetStatus::Degraded),
        (TargetStatus::Cloning, TargetStatus::Excluded),
        (TargetStatus::Cloning, TargetStatus::Deleting),
        (TargetStatus::Ready, TargetStatus::Fetching),
        (TargetStatus::Ready, TargetStatus::Degraded),
        (TargetStatus::Ready, TargetStatus::Paused),
        (TargetStatus::Ready, TargetStatus::Excluded),
        (TargetStatus::Ready, TargetStatus::Deleting),
        (TargetStatus::Fetching, TargetStatus::Snapshotting),
        (TargetStatus::Fetching, TargetStatus::Degraded),
        (TargetStatus::Fetching, TargetStatus::Paused),
        (TargetStatus::Fetching, TargetStatus::Excluded),
        (TargetStatus::Fetching, TargetStatus::Deleting),
        (TargetStatus::Snapshotting, TargetStatus::Verifying),
        (TargetStatus::Snapshotting, TargetStatus::Degraded),
        (TargetStatus::Snapshotting, TargetStatus::Deleting),
        (TargetStatus::Verifying, TargetStatus::Healthy),
        (TargetStatus::Verifying, TargetStatus::Degraded),
        (TargetStatus::Verifying, TargetStatus::Deleting),
        (TargetStatus::Healthy, TargetStatus::Fetching),
        (TargetStatus::Healthy, TargetStatus::Degraded),
        (TargetStatus::Healthy, TargetStatus::Paused),
        (TargetStatus::Healthy, TargetStatus::Excluded),
        (TargetStatus::Healthy, TargetStatus::Deleting),
        (TargetStatus::Degraded, TargetStatus::Fetching),
        (TargetStatus::Degraded, TargetStatus::Paused),
        (TargetStatus::Degraded, TargetStatus::Excluded),
        (TargetStatus::Degraded, TargetStatus::Deleting),
        (TargetStatus::Paused, TargetStatus::Ready),
        (TargetStatus::Paused, TargetStatus::Excluded),
        (TargetStatus::Paused, TargetStatus::Deleting),
        (TargetStatus::Excluded, TargetStatus::Requested),
        (TargetStatus::Excluded, TargetStatus::Deleting),
    ];

    /// Whether the state machine allows a direct move from one status to another.
    ///
    /// The check consults the closed transition table encoded in this module, so no move outside the design matrix can reach persistence.
    #[must_use]
    pub fn is_legal(from: TargetStatus, to: TargetStatus) -> bool {
        Self::TRANSITIONS
            .iter()
            .any(|&(allowed_from, allowed_to)| allowed_from == from && allowed_to == to)
    }
}

#[cfg(test)]
mod tests {
    use super::{TargetStatus, Transition};

    #[test]
    fn status_vocabulary_is_the_closed_set_of_eleven_states() {
        let expected = [
            "requested",
            "cloning",
            "ready",
            "fetching",
            "snapshotting",
            "verifying",
            "healthy",
            "degraded",
            "paused",
            "excluded",
            "deleting",
        ];
        let actual: Vec<&str> = TargetStatus::ALL
            .iter()
            .map(|status| status.as_str())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn transition_table_matches_design_matrix() {
        let legal_pairs = [
            (TargetStatus::Requested, TargetStatus::Cloning),
            (TargetStatus::Requested, TargetStatus::Excluded),
            (TargetStatus::Requested, TargetStatus::Deleting),
            (TargetStatus::Cloning, TargetStatus::Ready),
            (TargetStatus::Cloning, TargetStatus::Degraded),
            (TargetStatus::Cloning, TargetStatus::Excluded),
            (TargetStatus::Cloning, TargetStatus::Deleting),
            (TargetStatus::Ready, TargetStatus::Fetching),
            (TargetStatus::Ready, TargetStatus::Degraded),
            (TargetStatus::Ready, TargetStatus::Paused),
            (TargetStatus::Ready, TargetStatus::Excluded),
            (TargetStatus::Ready, TargetStatus::Deleting),
            (TargetStatus::Fetching, TargetStatus::Snapshotting),
            (TargetStatus::Fetching, TargetStatus::Degraded),
            (TargetStatus::Fetching, TargetStatus::Paused),
            (TargetStatus::Fetching, TargetStatus::Excluded),
            (TargetStatus::Fetching, TargetStatus::Deleting),
            (TargetStatus::Snapshotting, TargetStatus::Verifying),
            (TargetStatus::Snapshotting, TargetStatus::Degraded),
            (TargetStatus::Snapshotting, TargetStatus::Deleting),
            (TargetStatus::Verifying, TargetStatus::Healthy),
            (TargetStatus::Verifying, TargetStatus::Degraded),
            (TargetStatus::Verifying, TargetStatus::Deleting),
            (TargetStatus::Healthy, TargetStatus::Fetching),
            (TargetStatus::Healthy, TargetStatus::Degraded),
            (TargetStatus::Healthy, TargetStatus::Paused),
            (TargetStatus::Healthy, TargetStatus::Excluded),
            (TargetStatus::Healthy, TargetStatus::Deleting),
            (TargetStatus::Degraded, TargetStatus::Fetching),
            (TargetStatus::Degraded, TargetStatus::Paused),
            (TargetStatus::Degraded, TargetStatus::Excluded),
            (TargetStatus::Degraded, TargetStatus::Deleting),
            (TargetStatus::Paused, TargetStatus::Ready),
            (TargetStatus::Paused, TargetStatus::Excluded),
            (TargetStatus::Paused, TargetStatus::Deleting),
            (TargetStatus::Excluded, TargetStatus::Requested),
            (TargetStatus::Excluded, TargetStatus::Deleting),
        ];
        assert_eq!(legal_pairs.len(), 37);

        let all_ordered_pairs: Vec<_> = TargetStatus::ALL
            .iter()
            .flat_map(|&from| TargetStatus::ALL.iter().map(move |&to| (from, to)))
            .collect();
        let wrongly_rejected: Vec<_> = all_ordered_pairs
            .iter()
            .copied()
            .filter(|&(from, to)| {
                legal_pairs.contains(&(from, to)) && !Transition::is_legal(from, to)
            })
            .map(|(from, to)| (from.as_str(), to.as_str()))
            .collect();
        let wrongly_accepted: Vec<_> = all_ordered_pairs
            .iter()
            .copied()
            .filter(|&(from, to)| {
                !legal_pairs.contains(&(from, to)) && Transition::is_legal(from, to)
            })
            .map(|(from, to)| (from.as_str(), to.as_str()))
            .collect();
        assert!(
            wrongly_rejected.is_empty(),
            "legal pairs rejected: {wrongly_rejected:?}"
        );
        assert!(
            wrongly_accepted.is_empty(),
            "illegal pairs accepted: {wrongly_accepted:?}"
        );
    }
}
