//! The `ratatoskr-vault` service library: the reconciliation cycle the deployable runs.
//!
//! Kept separate from the binary so integration tests drive the real cycle end to end.

pub mod lfs_collection;
pub mod mirror_lifecycle;
pub mod policy_bus;
pub mod reconcile;
pub mod replication;
pub mod restore_verification;
pub mod retention;
pub mod snapshot_lifecycle;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod wiki;
