//! Typed execution boundary for the explicitly configured Git LFS binary.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::ConfinedPath;

/// A Git LFS operation Vault may execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfsOperation {
    /// Report the Git LFS client version.
    Version,
    /// Download every object reachable from every ref.
    FetchAll,
    /// Enumerate every referenced object with its full identifier.
    ListAllLong,
    /// Verify locally stored LFS objects.
    FsckObjects,
}

/// Trusted transfer adapter selected from the already validated source transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfsTransferMode {
    /// Built-in HTTP basic transfer only; repository custom adapters are disabled.
    Basic,
    /// Git LFS's built-in standalone file adapter for confined `file://` sources.
    StandaloneFile,
}

impl LfsOperation {
    fn arguments(self) -> Vec<OsString> {
        match self {
            Self::Version => vec![OsString::from("version")],
            Self::FetchAll => vec![
                OsString::from("fetch"),
                OsString::from("--all"),
                OsString::from("origin"),
            ],
            Self::ListAllLong => vec![
                OsString::from("ls-files"),
                OsString::from("--all"),
                OsString::from("--long"),
            ],
            Self::FsckObjects => vec![OsString::from("fsck"), OsString::from("--objects")],
        }
    }
}

/// Trusted process and confinement settings for one LFS run.
#[derive(Debug, Clone)]
pub struct LfsRunConfig {
    /// Absolute path to the trusted `git-lfs` executable.
    pub lfs_binary: PathBuf,
    /// Bare repository whose refs and remote are inspected.
    pub mirror_directory: ConfinedPath,
    /// Run-owned directory used as `lfs.storage`.
    pub storage_directory: ConfinedPath,
    /// Run-owned home directory for the child.
    pub run_home: ConfinedPath,
    /// Fixed built-in transfer mode selected from the validated source.
    pub transfer_mode: LfsTransferMode,
    /// Wall-clock deadline for every invocation.
    pub deadline: std::time::Duration,
    /// Retained standard-output limit.
    pub stdout_cap_bytes: usize,
    /// Retained standard-error limit.
    pub stderr_cap_bytes: usize,
}

/// A confined Git LFS runner.
#[derive(Debug, Clone)]
pub struct LfsRunner {
    config: LfsRunConfig,
}

impl LfsRunner {
    /// Creates a runner from trusted static configuration.
    #[must_use]
    pub const fn new(config: LfsRunConfig) -> Self {
        Self { config }
    }

    /// The exact opaque argument words passed to the configured executable.
    #[must_use]
    pub fn command_preview(operation: LfsOperation) -> Vec<OsString> {
        operation.arguments()
    }

    /// Runs one typed Git LFS operation.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal for invalid configuration, spawn failure, timeout, or output cap.
    pub async fn run(&self, operation: LfsOperation) -> Result<crate::RunOutcome, LfsRunnerError> {
        let process = self.process_config(operation)?;
        crate::spawn::run_bounded(process)
            .await
            .map_err(LfsRunnerError::from)
    }

    fn process_config(
        &self,
        operation: LfsOperation,
    ) -> Result<crate::spawn::ProcessConfig, LfsRunnerError> {
        if !self.config.lfs_binary.is_absolute() {
            return Err(LfsRunnerError::InvalidConfiguration {
                reason: "Git LFS binary must be an absolute path".to_owned(),
            });
        }
        revalidate(&self.config.mirror_directory)?;
        revalidate(&self.config.storage_directory)?;
        revalidate(&self.config.run_home)?;
        crate::credentials::create_private_dir_all(self.config.storage_directory.as_path())
            .map_err(|source| LfsRunnerError::InvalidConfiguration {
                reason: format!("cannot create confined LFS storage: {source}"),
            })?;
        crate::credentials::create_private_dir_all(self.config.run_home.as_path()).map_err(
            |source| LfsRunnerError::InvalidConfiguration {
                reason: format!("cannot create confined LFS home: {source}"),
            },
        )?;

        let environment = crate::env::lfs_child_environment(
            self.config
                .lfs_binary
                .parent()
                .unwrap_or(std::path::Path::new("/usr/bin")),
            self.config.run_home.as_path(),
            self.config.mirror_directory.as_path(),
            self.config.storage_directory.as_path(),
            self.config.transfer_mode == LfsTransferMode::StandaloneFile,
        );
        Ok(crate::spawn::ProcessConfig {
            binary: self.config.lfs_binary.clone(),
            arguments: operation.arguments(),
            working_directory: self.config.mirror_directory.as_path().to_path_buf(),
            environment,
            deadline: self.config.deadline,
            stdout_cap_bytes: self.config.stdout_cap_bytes,
            stderr_cap_bytes: self.config.stderr_cap_bytes,
            secrets: Vec::new(),
        })
    }

    /// Runs one operation while accepting a cooperative cancellation signal.
    ///
    /// # Errors
    ///
    /// Returns the same typed process errors as [`Self::run`].
    pub async fn run_cancellable(
        &self,
        operation: LfsOperation,
        cancellation: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<crate::RunOutcome, LfsRunnerError> {
        let process = self.process_config(operation)?;
        crate::spawn::run_bounded_cancellable(process, cancellation)
            .await
            .map_err(LfsRunnerError::from)
    }
}

fn revalidate(path: &ConfinedPath) -> Result<(), LfsRunnerError> {
    let relative = path.as_path().strip_prefix(path.root()).map_err(|_| {
        LfsRunnerError::InvalidConfiguration {
            reason: "confined path no longer belongs to its root".to_owned(),
        }
    })?;
    ConfinedPath::new(path.root(), relative)
        .map(|_| ())
        .map_err(|error| LfsRunnerError::InvalidConfiguration {
            reason: error.to_string(),
        })
}

/// Why a typed Git LFS operation did not produce bounded evidence.
#[derive(Debug, thiserror::Error)]
pub enum LfsRunnerError {
    /// Cooperative cancellation stopped the process group.
    #[error("Git LFS invocation was interrupted")]
    Interrupted,
    /// Static trusted input or a use-time path check failed.
    #[error("invalid Git LFS runner configuration: {reason}")]
    InvalidConfiguration {
        /// Safe diagnostic text.
        reason: String,
    },
    /// The configured executable could not start or be waited for.
    #[error("Git LFS binary could not be executed: {source}")]
    SpawnFailed {
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// The child exceeded its deadline and its process group was killed.
    #[error("Git LFS invocation exceeded the {}s deadline", deadline.as_secs())]
    Timeout {
        /// Configured deadline.
        deadline: std::time::Duration,
    },
    /// A child stream exceeded its retained-output cap.
    #[error("Git LFS exceeded the {stream} cap with {retained} retained bytes")]
    OutputLimitExceeded {
        /// Stream that exceeded its cap.
        stream: crate::outcome::Stream,
        /// Bytes retained under the cap.
        retained: usize,
    },
}

impl From<crate::spawn::ProcessError> for LfsRunnerError {
    fn from(error: crate::spawn::ProcessError) -> Self {
        match error {
            crate::spawn::ProcessError::Spawn(source) => Self::SpawnFailed { source },
            crate::spawn::ProcessError::Timeout(deadline) => Self::Timeout { deadline },
            crate::spawn::ProcessError::Interrupted => Self::Interrupted,
            crate::spawn::ProcessError::OutputLimit { stream, retained } => {
                Self::OutputLimitExceeded { stream, retained }
            }
        }
    }
}
