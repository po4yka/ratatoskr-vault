//! Typed failures of Git execution.
//!
//! Every refusal this crate makes before spawning (allowlist, URL, confinement) is its own
//! variant so callers and telemetry can distinguish "Vault refused" from "Git failed".

/// Why a Git invocation did not produce a bounded outcome.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitRunnerError {
    /// The requested subcommand is not enabled by the runner configuration. Raised before any
    /// process starts.
    #[error("subcommand `{requested}` is not enabled by the runner configuration")]
    DisallowedSubcommand {
        /// The literal subcommand word that was requested.
        requested: String,
    },

    /// The Git binary could not be started at all (missing binary, exec failure).
    #[error("git binary could not be started: {source}")]
    SpawnFailed {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A source URL failed validation (option injection or a transport outside the allowlist).
    /// Raised before any process starts.
    #[error("source URL is not acceptable: {reason}")]
    InvalidSourceUrl {
        /// Why the URL was refused, safe for logs and operator display.
        reason: String,
    },

    /// A filesystem path would resolve outside its Vault-owned root. Raised before any process
    /// starts.
    #[error("path is outside Vault confinement: {reason}")]
    PathOutsideConfinement {
        /// Why the path was refused, safe for logs and operator display.
        reason: String,
    },

    /// The child exceeded the wall-clock deadline; it and its whole process group were killed.
    #[error("git invocation exceeded the {}s deadline and was killed", deadline.as_secs())]
    Timeout {
        /// The deadline that elapsed.
        deadline: std::time::Duration,
    },

    /// A stream wrote past its cap; the child was killed and the retained bytes are bounded by
    /// the cap.
    #[error(
        "git wrote past the {stream} cap; {retained} bytes retained before the child was killed"
    )]
    OutputLimitExceeded {
        /// Which stream overflowed.
        stream: crate::outcome::Stream,
        /// How many bytes were retained, always at most the configured cap.
        retained: usize,
    },

    /// Credential material could not be staged for the operation.
    #[error("credential staging failed: {reason}")]
    CredentialWriteFailed {
        /// Why staging failed, safe for logs (never carries the material itself).
        reason: String,
    },

    /// An integrity tool reported a broken object database; structurally non-retryable to
    /// success.
    #[error("{tool} reported integrity failure: {excerpt}")]
    IntegrityCheckFailed {
        /// Which verification tool failed (`fsck` today; bundle verify arrives with snapshots).
        tool: &'static str,
        /// A bounded excerpt of the tool's diagnostics.
        excerpt: String,
    },
}
