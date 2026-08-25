//! Vault's confined execution of the system Git binary.
//!
//! Every Git process Vault ever starts goes through this crate. The design goals, in order:
//!
//! 1. **No shell anywhere.** Commands are typed argument vectors against a closed subcommand
//!    allowlist; URLs and paths flow through validated wrappers or the request is refused before
//!    any process exists.
//! 2. **Confinement.** Filesystem destinations resolve, at use time, inside a Vault-owned root;
//!    intermediate symlinks leaving the root are refused; mirror paths derive from internal
//!    identifiers only.
//! 3. **Hardening.** Children get a constructed environment (no inheritance), system and global
//!    Git configuration excluded, prompting and askpass disabled, hooks pointed at a non-directory.
//! 4. **Bounds.** Wall-clock deadlines and per-stream output caps on every invocation; overruns
//!    kill the child's whole process group and are reported as typed errors.
//! 5. **Secrets out of band.** Credential material reaches Git only through the credential-helper
//!    contract backed by an owner-only file in an owner-only run directory; captured output is
//!    redacted before it leaves this crate.
//!
//! The hostile-repository test suite under `tests/` proves each property against generated,
//! deterministic fixtures.

pub mod command;
pub mod confine;
mod credentials;
pub mod env;
pub mod error;
pub mod outcome;
mod spawn;
pub mod url;

pub use command::{GitOperation, Subcommand};
pub use confine::ConfinedPath;
pub use credentials::Credentials;
pub use error::GitRunnerError;
pub use outcome::RunOutcome;
pub use url::SourceUrl;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Safe `-c key=value` overrides prepended to every invocation so repository-local
/// configuration can never re-enable what Vault disables.
///
/// - `core.hooksPath=/dev/null` — `/dev/null` is not a directory, so every hook lookup fails
///   closed regardless of what the repository or any template configured;
/// - `core.hooksPath` beats repo config because `-c` has the highest precedence below forced
///   command-line flags;
/// - `protocol.ext.allow=never`-class hardening arrives with the transport work that needs it;
///   transports are already closed at URL validation time.
const SAFE_CONFIG_WORDS: [&str; 2] = ["-c", "core.hooksPath=/dev/null"];

/// Static configuration for one runner instance.
///
/// Everything here is trusted operator input: the Git binary location and which subcommands this
/// instance may run. Runtime per-request data (URLs, paths) is validated separately.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Path to the trusted system Git binary.
    pub git_binary: PathBuf,
    /// The subcommands this runner instance is allowed to execute.
    pub allowed: Vec<Subcommand>,
    /// The working directory every invocation starts in (a confined Vault-owned path).
    pub working_directory: PathBuf,
    /// The per-run home directory handed to children as `HOME` (inside Vault confinement).
    pub run_home: PathBuf,
    /// The wall-clock deadline for every invocation.
    pub deadline: std::time::Duration,
    /// Retained stdout limit for every invocation.
    pub stdout_cap_bytes: usize,
    /// Retained stderr limit for every invocation.
    pub stderr_cap_bytes: usize,
    /// Path to the shipped credential-helper binary (trusted configuration).
    pub credential_helper: PathBuf,
}

/// Executes allowlisted Git commands as confined subprocesses.
#[derive(Debug, Clone)]
pub struct GitRunner {
    config: RunConfig,
}

impl GitRunner {
    /// Creates a runner from its static configuration.
    #[must_use]
    pub const fn new(config: RunConfig) -> Self {
        Self { config }
    }

    /// The exact argument vector this runner would execute for `op`.
    ///
    /// An audit surface: redacted logging and tests read it instead of guessing. When the
    /// operation carries credentials, the credential words appear with a `<staged-secret>`
    /// placeholder where run time substitutes the per-operation secret file path.
    #[must_use]
    pub fn command_preview(&self, op: &GitOperation) -> Vec<std::ffi::OsString> {
        Self::command_words(self.config.credential_helper.as_path(), op, None)
    }

    /// Builds the full argument vector: safe config overrides, then optional credential-helper
    /// wiring, then the subcommand and its typed arguments.
    ///
    /// The helper value is a single `-c credential.helper=<helper> <secret-file>` pair; Git
    /// word-splits it and execvp's it directly without a shell. `staged_secret` is `None` for
    /// previews (placeholder substituted) and the real owner-only file at run time.
    fn command_words(
        helper: &Path,
        op: &GitOperation,
        staged_secret: Option<&Path>,
    ) -> Vec<std::ffi::OsString> {
        let mut words: Vec<std::ffi::OsString> = SAFE_CONFIG_WORDS
            .iter()
            .map(std::ffi::OsString::from)
            .collect();

        if op.credentials().is_some() {
            let secret = staged_secret.map_or_else(
                || std::ffi::OsString::from("<staged-secret>"),
                |path| path.as_os_str().to_os_string(),
            );
            let mut helper_value = std::ffi::OsString::from("credential.helper=");
            helper_value.push(helper);
            helper_value.push(std::ffi::OsString::from(" "));
            helper_value.push(secret);
            words.push(std::ffi::OsString::from("-c"));
            words.push(helper_value);
        }

        words.push(std::ffi::OsString::from(op.subcommand().as_str()));
        words.extend(op.arguments().iter().cloned());
        words
    }

    /// Runs one allowlisted Git operation to completion.
    ///
    /// # Errors
    ///
    /// Returns [`GitRunnerError::DisallowedSubcommand`] when the configuration does not enable
    /// the operation's subcommand, [`GitRunnerError::CredentialWriteFailed`] when credential
    /// staging fails, and [`GitRunnerError::SpawnFailed`] when the binary cannot be started.
    #[expect(
        clippy::too_many_lines,
        reason = "the spawn loop concentrates every process-handling decision in one auditable place"
    )]
    pub async fn run(&self, op: &GitOperation) -> Result<RunOutcome, GitRunnerError> {
        if !self.config.allowed.contains(&op.subcommand()) {
            return Err(GitRunnerError::DisallowedSubcommand {
                requested: op.subcommand().as_str().to_owned(),
            });
        }

        // HOME must exist before the child starts; Git only reads it, but a missing home makes
        // some helpers misbehave. Created best-effort inside confinement.
        let _ignored = std::fs::create_dir_all(&self.config.run_home);

        // Credential staging happens before anything else that can fail differently, so the
        // cleanup guard covers every later path by construction.
        let staged = match op.credentials() {
            Some(credentials) => Some(credentials::prepare_secret_file(
                &self.config.run_home,
                credentials,
            )?),
            None => None,
        };
        let _cleanup = credentials::SecretCleanup(staged.clone());

        let words = Self::command_words(
            self.config.credential_helper.as_path(),
            op,
            staged.as_deref(),
        );
        let mut command = Command::new(&self.config.git_binary);
        for word in words {
            command.arg(word);
        }
        let environment = env::child_environment(
            self.config
                .git_binary
                .parent()
                .unwrap_or(Path::new("/usr/bin")),
            &self.config.run_home,
        );

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&self.config.working_directory)
            .env_clear()
            .envs(&environment)
            .kill_on_drop(true);

        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .map_err(|source| GitRunnerError::SpawnFailed { source })?;
        let pid = child.id();
        let started = std::time::Instant::now();
        // Armed for the whole call: a completed run finds an empty group, a cancelled future
        // still kills every descendant.
        let _group_guard = spawn::GroupGuard(pid);

        // Held open so children that read stdin block instead of seeing EOF and exiting; the
        // guard drops it after the wait below.
        let _stdin_hold = child.stdin.take();

        let stdout_pipe = spawn::boxed_reader(child.stdout.take());
        let stderr_pipe = spawn::boxed_reader(child.stderr.take());
        let stdout_reader = tokio::spawn(spawn::read_capped(
            stdout_pipe,
            self.config.stdout_cap_bytes,
        ));
        let stderr_reader = tokio::spawn(spawn::read_capped(
            stderr_pipe,
            self.config.stderr_cap_bytes,
        ));

        let waited = tokio::time::timeout(self.config.deadline, child.wait()).await;

        match waited {
            Err(_elapsed) => {
                spawn::kill_group(pid);
                // Reap so the killed child cannot linger as a zombie.
                let _reaped = child.wait().await;
                Err(GitRunnerError::Timeout {
                    deadline: self.config.deadline,
                })
            }
            Ok(Err(source)) => Err(GitRunnerError::SpawnFailed { source }),
            Ok(Ok(status)) => {
                let reader_failed = || GitRunnerError::SpawnFailed {
                    source: std::io::Error::other("stream reader task failed"),
                };
                let (stdout, stdout_overflow) = stdout_reader.await.map_err(|_| reader_failed())?;
                let (stderr, stderr_overflow) = stderr_reader.await.map_err(|_| reader_failed())?;
                if stdout_overflow || stderr_overflow {
                    // The writer may still be alive trying to push bytes; end it and its group.
                    spawn::kill_group(pid);
                    let _reaped = child.wait().await;
                    let (stream, retained) = if stdout_overflow {
                        (outcome::Stream::Stdout, stdout.len())
                    } else {
                        (outcome::Stream::Stderr, stderr.len())
                    };
                    return Err(GitRunnerError::OutputLimitExceeded { stream, retained });
                }
                // Redact once, use everywhere: streams and excerpts both leave the crate.
                let secrets = op
                    .credentials()
                    .map(credentials::Credentials::secret_bytes)
                    .unwrap_or_default();
                let stdout = outcome::redact_secrets(stdout, &secrets);
                let stderr = outcome::redact_secrets(stderr, &secrets);

                // A failing integrity tool is structurally non-retryable to success; it must
                // never surface as a plain exit code a caller could ignore. Operation-level
                // failure mapping widens with the mirror lifecycle.
                if status.code().is_some_and(|code| code != 0)
                    && op.subcommand() == Subcommand::Fsck
                {
                    return Err(GitRunnerError::IntegrityCheckFailed {
                        tool: "fsck",
                        excerpt: outcome::bounded_excerpt(&stderr, 512),
                    });
                }

                Ok(RunOutcome {
                    exit_code: status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                    stdout_truncated: stdout_overflow,
                    stderr_truncated: stderr_overflow,
                    duration: started.elapsed(),
                })
            }
        }
    }
}
