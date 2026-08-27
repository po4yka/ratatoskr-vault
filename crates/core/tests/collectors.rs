//! Approval gate for executable auxiliary collectors.

use ratatoskr_vault_core::collectors::{CollectorKind, UnsupportedCollector};
use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::planner::{DivergenceKind, plan};
use ratatoskr_vault_core::target_state::TargetStatus;

#[test]
fn unapproved_provider_collectors_are_non_executable() {
    let inventory: Vec<_> = CollectorKind::ALL
        .iter()
        .map(|collector| collector.as_str())
        .collect();
    assert_eq!(inventory, ["git_lfs", "wiki"]);

    let desired = ValidatedDelivery {
        preservation_level: "complete_archive".to_owned(),
        pinned: Some(false),
        include_wiki: Some(false),
        include_releases: Some(true),
        include_issues: Some(true),
        offsite_required: Some(false),
        correlation_id: "approval-gate".to_owned(),
        policy_revision: Some(1),
    };
    let planned = plan(&desired, Some(TargetStatus::Ready));

    assert!(planned.divergences.iter().any(|divergence| {
        divergence.kind == DivergenceKind::UnsupportedCollector(UnsupportedCollector::Releases)
    }));
    assert!(planned.divergences.iter().any(|divergence| {
        divergence.kind == DivergenceKind::UnsupportedCollector(UnsupportedCollector::Issues)
    }));
}
