//! The pure reconciliation planner: desired state in, work items out.
//!
//! `plan()` is `f(desired, observed) -> ReconciliationPlan`: no I/O, no clock, no database. The
//! executor decides what it can perform today; forward-looking items stay visible as
//! planned-not-executable work instead of being silently dropped (design D5).

use crate::collectors::UnsupportedCollector;
use crate::delivery::ValidatedDelivery;
use crate::target_state::TargetStatus;

/// One unit of reconciliation work the planner proposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkItem {
    /// Create the target and bring it under management from scratch.
    Enroll,
    /// Withdraw an actively managed target whose governing policy became inactive.
    Exclude,
    /// Return an excluded target to management under a renewed active policy.
    Reactivate,
    /// Persist evidence that the newest revision was seen; desired and observed already agree,
    /// so no state change is proposed.
    RecordRevisionOnly,
    /// Run the confined Git clone or fetch so the mirror satisfies the governing level.
    /// Forward-looking: no runner exists yet, reported planned-not-executable.
    CloneAndFetch,
    /// Build, verify, and publish an immutable snapshot from the mirror. Forward-looking:
    /// no runner exists yet, reported planned-not-executable.
    Snapshot,
}

impl WorkItem {
    /// Whether the current executor can perform this item itself. Forward-looking runner work
    /// stays planned-not-executable until its runners exist; it is reported, never faked
    /// (design D5).
    #[must_use]
    pub const fn is_executable(self) -> bool {
        matches!(
            self,
            Self::Enroll | Self::Exclude | Self::Reactivate | Self::RecordRevisionOnly
        )
    }
}

/// Structured counts summarizing one plan for logs and health reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanCounters {
    /// Items proposed in total, executable and forward-looking alike.
    pub proposed: usize,
    /// Items the current executor can actually perform.
    pub executable: usize,
}

/// A recorded disagreement between governing desire and observed state that the planner
/// deliberately leaves unactioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    /// What disagrees, using the closed divergence vocabulary.
    pub kind: DivergenceKind,
}

/// The closed vocabulary of deliberate non-actions the planner records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DivergenceKind {
    /// The governing policy withdrew, but the pinned target keeps its protection: exclusion is
    /// blocked and the disagreement stays observable instead of being silently dropped.
    PinnedWithdrawal,
    /// A desired provider collector is known but has not been separately approved for execution.
    UnsupportedCollector(UnsupportedCollector),
}

/// The planner's complete proposal for one convergence pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationPlan {
    /// Work items in proposal order.
    pub items: Vec<WorkItem>,
    /// Disagreements recorded instead of acted on.
    pub divergences: Vec<Divergence>,
    /// Aggregated counts over [`Self::items`].
    pub counters: PlanCounters,
}

/// Plans the work that moves actual state toward the governing desired state.
///
/// Branches arrive with their tests. With no observed target, only an active policy — a
/// `preservation_level` other than `none` — enrolls one. A governing `none` withdraws an
/// unpinned target; a pinned one survives with its disagreement recorded as a [`Divergence`],
/// never silently dropped. An omitted `pinned` flag resolves to unpinned, the same default the
/// validation boundary documents for its consumers.
#[must_use]
pub fn plan(desired: &ValidatedDelivery, observed: Option<TargetStatus>) -> ReconciliationPlan {
    let pinned = matches!(desired.pinned, Some(true));
    let withdrawn = desired.preservation_level == "none";
    let mut items = Vec::new();
    let mut divergences = Vec::new();
    if matches!(desired.include_releases, Some(true)) {
        divergences.push(Divergence {
            kind: DivergenceKind::UnsupportedCollector(UnsupportedCollector::Releases),
        });
    }
    if matches!(desired.include_issues, Some(true)) {
        divergences.push(Divergence {
            kind: DivergenceKind::UnsupportedCollector(UnsupportedCollector::Issues),
        });
    }
    match observed {
        None if !withdrawn => items.push(WorkItem::Enroll),
        Some(TargetStatus::Excluded) if withdrawn => {}
        Some(_) if withdrawn => {
            if pinned {
                divergences.push(Divergence {
                    kind: DivergenceKind::PinnedWithdrawal,
                });
            } else {
                items.push(WorkItem::Exclude);
            }
        }
        Some(TargetStatus::Excluded) => items.push(WorkItem::Reactivate),
        Some(TargetStatus::Ready) => {
            if desired.preservation_level == "git_mirror_with_lfs" {
                items.push(WorkItem::CloneAndFetch);
                items.push(WorkItem::Snapshot);
            } else {
                items.push(WorkItem::RecordRevisionOnly);
            }
        }
        _ => {}
    }
    let counters = PlanCounters {
        proposed: items.len(),
        executable: items.iter().filter(|item| item.is_executable()).count(),
    };
    ReconciliationPlan {
        items,
        divergences,
        counters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_policy() -> ValidatedDelivery {
        ValidatedDelivery {
            preservation_level: "git_mirror".to_owned(),
            pinned: Some(false),
            include_wiki: Some(false),
            include_releases: Some(false),
            include_issues: Some(false),
            offsite_required: Some(true),
            correlation_id: "corr-enroll".to_owned(),
            policy_revision: Some(1),
        }
    }

    #[test]
    fn first_active_policy_enrolls_a_new_target() {
        let plan = plan(&active_policy(), None);

        assert_eq!(plan.items, vec![WorkItem::Enroll]);
    }

    #[test]
    fn none_policy_excludes_unpinned_active_target() {
        let mut withdrawal = active_policy();
        withdrawal.preservation_level = "none".to_owned();

        let plan = plan(&withdrawal, Some(TargetStatus::Ready));

        assert_eq!(plan.items, vec![WorkItem::Exclude]);
    }

    #[test]
    fn pinned_target_survives_withdrawal() {
        let mut withdrawal = active_policy();
        withdrawal.preservation_level = "none".to_owned();
        withdrawal.pinned = Some(true);

        let plan = plan(&withdrawal, Some(TargetStatus::Ready));

        assert_eq!(plan.items, Vec::<WorkItem>::new());
        assert_eq!(
            plan.divergences,
            vec![Divergence {
                kind: DivergenceKind::PinnedWithdrawal
            }]
        );
    }

    #[test]
    fn excluded_target_reactivates_on_new_active_policy() {
        let plan = plan(&active_policy(), Some(TargetStatus::Excluded));

        assert_eq!(plan.items, vec![WorkItem::Reactivate]);
    }

    #[test]
    fn unchanged_newest_revision_records_evidence_only() {
        let plan = plan(&active_policy(), Some(TargetStatus::Ready));

        assert_eq!(plan.items, vec![WorkItem::RecordRevisionOnly]);
    }

    #[test]
    fn second_convergence_pass_is_a_no_op() {
        let mut withdrawal = active_policy();
        withdrawal.preservation_level = "none".to_owned();

        let after_exclude = plan(&withdrawal, Some(TargetStatus::Excluded));
        assert_eq!(after_exclude.items, Vec::<WorkItem>::new());

        let after_enrollment = plan(&active_policy(), Some(TargetStatus::Requested));
        assert_eq!(after_enrollment.items, Vec::<WorkItem>::new());
    }

    #[test]
    fn runner_work_is_reported_planned_not_executable() {
        let mut lfs_policy = active_policy();
        lfs_policy.preservation_level = "git_mirror_with_lfs".to_owned();

        let plan = plan(&lfs_policy, Some(TargetStatus::Ready));

        assert_eq!(
            plan.items,
            vec![WorkItem::CloneAndFetch, WorkItem::Snapshot]
        );
        assert!(plan.items.iter().all(|item| !item.is_executable()));
        assert_eq!(plan.counters.proposed, 2);
        assert_eq!(plan.counters.executable, 0);
    }
}
