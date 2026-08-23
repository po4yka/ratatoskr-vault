//! The error taxonomy of Vault: which part failed, and how a backup failure is classified.
//!
//! Two closed sets live here. [`Subsystem`] names the part of the process; it is telemetry
//! vocabulary with bounded cardinality and never reaches a client. [`FailureClass`] classifies a
//! backup-domain failure along the three groups `AGENTS.md` defines — transient, action-required,
//! integrity — so callers decide between retrying, alerting a human, and refusing to report
//! healthy. Integrity failures are structurally non-retryable-to-success: no amount of retrying
//! makes corrupt bytes verify.
//!
//! There is deliberately no client-facing fault envelope here. Vault serves an admin plane whose
//! bodies carry closed-vocabulary check states; the public API surface arrives at a later plan
//! item together with the wire contracts it must speak.

/// Which part of the process failed.
///
/// Bounded-cardinality telemetry only: never on a wire, never in a response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Subsystem {
    /// Reading or validating the typed configuration.
    Config,
    /// The subscriber, the exporter or an instrument.
    Telemetry,
    /// The HTTP harness: a listener, a middleware, or a handler.
    Http,
    /// The database pool, the schema, or a query.
    Persistence,
}

impl Subsystem {
    /// Every subsystem, so the label set can never grow silently. The array length is the
    /// documented count, so adding a variant without updating it does not compile.
    pub const ALL: [Self; 4] = [Self::Config, Self::Telemetry, Self::Http, Self::Persistence];

    /// The telemetry label and log field value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Telemetry => "telemetry",
            Self::Http => "http",
            Self::Persistence => "persistence",
        }
    }
}

impl core::fmt::Display for Subsystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How long a backup failure may be kept from a human.
///
/// The three groups `AGENTS.md` defines in "Failure handling". A group, not a boolean: the answer
/// to "what does the operator do about this" has exactly three shapes — wait, act, investigate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureGroup {
    /// Repeating the operation later can succeed without anybody changing anything.
    Transient,
    /// Somebody must change a credential, a quota, a policy, or the repository itself.
    ActionRequired,
    /// Stored bytes do not match their evidence. Never reported as healthy, whatever happens
    /// next (`docs/ARCHITECTURE.md` section 27, invariant 14).
    Integrity,
}

impl FailureGroup {
    /// Whether retrying the identical operation is a reasonable first response.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Transient)
    }
}

/// The closed classification of one backup-domain failure.
///
/// Every variant fixes a stable code and a group. Nothing here is derived from data, so a hostile
/// repository name or a provider message cannot mint a new class. Variants arrive as the features
/// that produce them; the set below is what `AGENTS.md` names today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureClass {
    // ---- Transient ----
    /// The network, the Git host or the object store did not answer.
    RemoteUnavailable,
    /// The provider asked us to slow down.
    RateLimited,
    /// A dependency this process needs (database, bus, storage backend) refused work.
    DependencyUnavailable,
    /// The process was interrupted mid-operation; state is diagnosable and resumable.
    Interrupted,

    // ---- Action required ----
    /// A credential was revoked, expired, or lacks the scope this operation needs.
    AuthRequired,
    /// The repository is gone, renamed beyond recognition, or inaccessible to the credential.
    RepositoryUnavailable,
    /// A disk quota or storage reservation was hit; continuing would exhaust the host.
    QuotaExceeded,
    /// The repository uses a configuration this policy does not preserve (an unsupported filter,
    /// an unsafe submodule).
    UnsupportedSource,

    // ---- Integrity ----
    /// `git fsck` found the mirror's object database damaged.
    MirrorCorruption,
    /// An artifact failed format verification (`git bundle verify`) or hash comparison.
    ArtifactVerificationFailed,
    /// The stored manifest does not match the artifact it describes.
    ManifestMismatch,
    /// Policy requires LFS and objects are missing; preserving pointers alone is not preservation.
    LfsIncomplete,
    /// The off-host copy differs from the local artifact after transfer.
    OffsiteMismatch,
    /// A restore drill could not reconstruct a usable repository from the artifact.
    RestoreDrillFailed,
    /// A path escaped, or tried to escape, its configured root.
    UnsafePath,
}

impl FailureClass {
    /// Every class, grouped transient-first. The array length is the documented count, so adding
    /// a variant without updating it does not compile.
    pub const ALL: [Self; 14] = [
        Self::RemoteUnavailable,
        Self::RateLimited,
        Self::DependencyUnavailable,
        Self::Interrupted,
        Self::AuthRequired,
        Self::RepositoryUnavailable,
        Self::QuotaExceeded,
        Self::UnsupportedSource,
        Self::MirrorCorruption,
        Self::ArtifactVerificationFailed,
        Self::ManifestMismatch,
        Self::LfsIncomplete,
        Self::OffsiteMismatch,
        Self::RestoreDrillFailed,
    ];

    /// The stable, machine-actionable code — the only member a consumer may branch on.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RemoteUnavailable => "vault.remote.unavailable",
            Self::RateLimited => "vault.remote.rate_limited",
            Self::DependencyUnavailable => "vault.dependency.unavailable",
            Self::Interrupted => "vault.operation.interrupted",
            Self::AuthRequired => "vault.auth.required",
            Self::RepositoryUnavailable => "vault.repository.unavailable",
            Self::QuotaExceeded => "vault.storage.quota_exceeded",
            Self::UnsupportedSource => "vault.source.unsupported",
            Self::MirrorCorruption => "vault.mirror.corrupt",
            Self::ArtifactVerificationFailed => "vault.artifact.verification_failed",
            Self::ManifestMismatch => "vault.manifest.mismatch",
            Self::LfsIncomplete => "vault.lfs.incomplete",
            Self::OffsiteMismatch => "vault.offsite.mismatch",
            Self::RestoreDrillFailed => "vault.restore.drill_failed",
            Self::UnsafePath => "vault.path.unsafe",
        }
    }

    /// The group the class belongs to. Exhaustive over [`FailureClass::ALL`] by construction: a
    /// new variant does not compile until it has chosen one, and choosing wrong is a reviewed
    /// diff away from choosing right.
    #[must_use]
    pub const fn group(self) -> FailureGroup {
        match self {
            Self::RemoteUnavailable
            | Self::RateLimited
            | Self::DependencyUnavailable
            | Self::Interrupted => FailureGroup::Transient,
            Self::AuthRequired
            | Self::RepositoryUnavailable
            | Self::QuotaExceeded
            | Self::UnsupportedSource => FailureGroup::ActionRequired,
            Self::MirrorCorruption
            | Self::ArtifactVerificationFailed
            | Self::ManifestMismatch
            | Self::LfsIncomplete
            | Self::OffsiteMismatch
            | Self::RestoreDrillFailed
            | Self::UnsafePath => FailureGroup::Integrity,
        }
    }
}

impl core::fmt::Display for FailureClass {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.code())
    }
}
