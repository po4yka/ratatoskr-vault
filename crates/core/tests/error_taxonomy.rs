//! The error taxonomy: closed sets, stable codes, honest groups. Spec: `service-runtime`
//! (failure classification is what makes "integrity failures never report healthy" checkable).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::error::{FailureClass, FailureGroup, Subsystem};
use std::collections::HashSet;

/// Every class carries a code, and no two classes share one: a consumer branching on a code must
/// never have to wonder which failure happened.
#[test]
fn every_failure_class_has_a_unique_stable_code() {
    let codes: HashSet<&str> = FailureClass::ALL.iter().map(|class| class.code()).collect();
    assert_eq!(
        codes.len(),
        FailureClass::ALL.len(),
        "two classes share a code"
    );
    for code in FailureClass::ALL.iter().map(|class| class.code()) {
        assert!(
            code.starts_with("vault."),
            "{code} must be namespaced under vault."
        );
    }
}

/// The array length is the documented count: adding a variant without updating `ALL` does not
/// compile, and this assertion is what the count means.
#[test]
fn the_documented_count_matches_all() {
    assert_eq!(FailureClass::ALL.len(), 14);
    assert_eq!(Subsystem::ALL.len(), 4);
}

/// Integrity failures are never retryable-to-success: no amount of retrying makes corrupt bytes
/// verify. Transient failures are the only group where retrying is the reasonable first response.
#[test]
fn only_transient_failures_are_retryable() {
    for class in FailureClass::ALL {
        let retryable = class.group().retryable();
        assert_eq!(
            retryable,
            class.group() == FailureGroup::Transient,
            "{class} must derive retryability from its group"
        );
        if class.group() == FailureGroup::Integrity {
            assert!(
                !retryable,
                "{class} is an integrity failure and must never claim a retry will fix it"
            );
        }
    }
}

/// Subsystem labels are telemetry vocabulary: lowercase, stable, bounded.
#[test]
fn subsystem_labels_are_closed_and_lowercase() {
    for subsystem in Subsystem::ALL {
        let label = subsystem.as_str();
        assert!(
            !label.is_empty() && label.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{label} must be a lowercase telemetry label"
        );
    }
}
