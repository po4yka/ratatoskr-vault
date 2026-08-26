//! Closed domain outcomes for a single mirror lifecycle run.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::error::FailureClass;
use ratatoskr_vault_core::mirror::{MirrorCheckpoint, MirrorOperation, MirrorResult};

#[test]
fn quota_refusal_and_interruption_have_closed_lifecycle_results() {
    let completed = MirrorResult::Succeeded {
        object_count: 7,
        bytes_on_disk: 1024,
    };
    let quota_refused = MirrorResult::QuotaRefused;
    let interrupted = MirrorResult::Interrupted {
        checkpoint: MirrorCheckpoint::FetchPending,
    };
    let integrity_failed = MirrorResult::IntegrityFailed;

    assert!(completed.is_success());
    assert_eq!(
        quota_refused.failure_class(),
        Some(FailureClass::QuotaExceeded)
    );
    assert_eq!(
        interrupted.failure_class(),
        Some(FailureClass::Interrupted),
        "an interrupted fetch retains a resumable checkpoint"
    );
    assert_eq!(
        integrity_failed.failure_class(),
        Some(FailureClass::MirrorCorruption)
    );
    assert_eq!(
        MirrorOperation::InitialClone.checkpoint(),
        MirrorCheckpoint::ClonePending
    );
}
