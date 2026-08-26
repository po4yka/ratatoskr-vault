//! Closed, persistence-neutral vocabulary for one mirror operation.
//!
//! The executor owns I/O and the database owns durable evidence. This module is intentionally
//! small: it ensures those layers cannot invent an unclassified success, quota refusal,
//! interruption, or integrity failure while threading a run through the state machine.

use crate::error::FailureClass;

/// The two safe Git operations Vault performs on a bare mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorOperation {
    /// First acquisition into a run-owned staging directory.
    InitialClone,
    /// Refresh of an already-published bare mirror.
    Fetch,
}

impl MirrorOperation {
    /// The checkpoint to retain when this operation is interrupted before successful evidence.
    #[must_use]
    pub const fn checkpoint(self) -> MirrorCheckpoint {
        match self {
            Self::InitialClone => MirrorCheckpoint::ClonePending,
            Self::Fetch => MirrorCheckpoint::FetchPending,
        }
    }
}

/// The bounded work that a later cycle may resume after interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorCheckpoint {
    /// No mirror was published, so a fresh staging clone remains necessary.
    ClonePending,
    /// The existing mirror remains usable; run the normal fetch again.
    FetchPending,
}

/// The complete outcome vocabulary of one admitted lifecycle run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorResult {
    /// Git work and all post-operation evidence succeeded.
    Succeeded {
        /// Connected object count measured after `git fsck`.
        object_count: u64,
        /// Byte usage measured after publication or refresh.
        bytes_on_disk: u64,
    },
    /// Admission rejected the work before Git could allocate or publish data.
    QuotaRefused,
    /// Cooperative cancellation stopped Git and retained the resumable unit of work.
    Interrupted {
        /// The normal operation a later cycle must retry.
        checkpoint: MirrorCheckpoint,
    },
    /// `fsck`, refs, or object-count sanity checks did not provide valid evidence.
    IntegrityFailed,
    /// Git or local storage failed without proving mirror corruption.
    Failed {
        /// The closed classification retained with the failed evidence.
        failure: FailureClass,
    },
}

impl MirrorResult {
    /// Whether this outcome establishes a new successful mirror observation.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    /// The stable failure classification, if the outcome did not succeed.
    #[must_use]
    pub const fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::Succeeded { .. } => None,
            Self::QuotaRefused => Some(FailureClass::QuotaExceeded),
            Self::Interrupted { .. } => Some(FailureClass::Interrupted),
            Self::IntegrityFailed => Some(FailureClass::MirrorCorruption),
            Self::Failed { failure } => Some(failure),
        }
    }
}
