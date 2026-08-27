//! The `ratatoskr-vault` service library: the reconciliation cycle the deployable runs.
//!
//! Kept separate from the binary so integration tests drive the real cycle end to end.

pub mod lfs_collection;
pub mod mirror_lifecycle;
pub mod reconcile;
pub mod replication;
pub mod restore_verification;
pub mod snapshot_lifecycle;
pub mod wiki;
