//! Typed configuration: the tree, the loader, and the startup rules.
//!
//! # Sources and precedence
//!
//! Two providers, lowest precedence first:
//!
//! 1. built-in defaults;
//! 2. `RATATOSKR__` environment variables, with `__` separating nesting levels, e.g.
//!    `RATATOSKR__TELEMETRY__OTLP__ENDPOINT`.
//!
//! # There is deliberately no configuration file
//!
//! One mechanism and one place to look. No search path, no provenance check, no rule that a secret
//! may not come from a file — none of which can be wrong if there is no file. The deployment model
//! is a container reading its environment. A lower-precedence file provider is a one-line addition
//! at the milestone an operator asks for one.
//!
//! What is not deferrable is the naming scheme: environment variable names are an operational
//! contract, and renaming them later breaks every deployment manifest in the fleet.

mod model;
mod validate;

use figment::Figment;
use figment::providers::{Env, Serialized};

pub use crate::config::model::{
    AdminConfig, DEFAULT_ADMIN_PORT, DatabaseConfig, LogFormat, MirrorLifecycleConfig, OtlpConfig,
    ReplicaTargetConfig, ReplicasConfig, ShutdownConfig, TelemetryConfig, VaultConfig,
    VerificationConfig,
};
pub use crate::config::validate::{SHUTDOWN_CEILING_SECONDS, Violation};

/// The environment prefix, and the nesting separator inside it.
const ENV_PREFIX: &str = "RATATOSKR__";

/// Reads the process environment and produces a validated configuration.
///
/// Sources, lowest precedence first: built-in defaults, then `RATATOSKR__` environment variables
/// with `__` separating nesting levels. There is no configuration file (see the module
/// documentation for why).
///
/// # Errors
///
/// [`ConfigError::Source`] when extraction fails — a wrong type or an unknown key. figment is
/// fail-fast, so this reports exactly one problem. [`ConfigError::Invalid`] carries EVERY semantic
/// violation found, never only the first, because an operator editing an environment wants one
/// round trip and not five.
#[allow(
    clippy::result_large_err,
    reason = "figment::Error is the specified payload of ConfigError::Source; boxing it would hide \
              the key it names behind an extra indirection for a value that is constructed once, \
              at startup, on the path that then exits"
)]
pub fn load() -> Result<VaultConfig, ConfigError> {
    load_from(figment())
}

/// The provider stack [`load`] uses, exposed so a test can add a provider on top of it.
#[must_use]
pub fn figment() -> Figment {
    Figment::from(Serialized::defaults(VaultConfig::defaults()))
        .merge(Env::prefixed(ENV_PREFIX).split("__"))
}

/// Extracts and validates from an arbitrary figment. The seam every configuration test uses.
///
/// # Errors
///
/// As [`load`].
#[allow(
    clippy::result_large_err,
    reason = "as `load`: the error payload is figment::Error by specification"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the figment is consumed so a caller cannot extract the same stack twice and reason \
              about two configurations in one process"
)]
pub fn load_from(figment: Figment) -> Result<VaultConfig, ConfigError> {
    let config: VaultConfig = figment.extract()?;
    let violations = validate::validate(&config);
    if violations.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid(violations))
    }
}

/// Every reason a Vault process must refuse to start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Extraction failed: a wrong type, a missing key, or an unknown key.
    ///
    /// figment's own message is deliberately NOT interpolated. It quotes the supplied value for
    /// several field types, so a `Display` that carried it would make one `tracing::error!(%error)`
    /// a live secret leak. [`ConfigError::report`] is the only operator-facing rendering, and it is
    /// value-free by construction.
    #[error("configuration could not be read")]
    Source(#[from] figment::Error),

    /// The configuration parsed but violates one or more startup rules. Carries every violation
    /// found.
    #[error("configuration is invalid: {} problem(s)", .0.len())]
    Invalid(Vec<Violation>),
}

impl ConfigError {
    /// The operator-facing report, written to stderr before any subscriber exists.
    /// One block per problem, stable order, no supplied values.
    #[must_use]
    pub fn report(&self) -> String {
        match self {
            Self::Source(error) => validate::report_unreadable(error),
            Self::Invalid(violations) => validate::report_invalid(violations),
        }
    }

    /// `78` — `EX_CONFIG` from `sysexits.h`. `systemctl status` renders it as `EXIT_CONFIG`, which
    /// distinguishes "your configuration is wrong" from "the process crashed" in a unit that is
    /// restarting every ten seconds.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        78
    }
}
