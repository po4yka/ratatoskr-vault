//! Closed executable collector vocabulary.

/// Collector kinds whose implementations are owner-approved and executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorKind {
    /// Git LFS object preservation.
    GitLfs,
    /// Repository wiki preservation through its sibling Git repository.
    Wiki,
}

impl CollectorKind {
    /// The current executable inventory.
    pub const ALL: [Self; 2] = [Self::GitLfs, Self::Wiki];

    /// Stable persisted name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitLfs => "git_lfs",
            Self::Wiki => "wiki",
        }
    }
}

/// Requested provider collectors that deliberately have no executable implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedCollector {
    /// GitHub releases and their assets.
    Releases,
    /// GitHub issues and comments.
    Issues,
}
