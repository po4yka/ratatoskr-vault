//! Per-operation credential materialization and cleanup.
//!
//! The design keeps secrets out of every argument vector and environment block by handing Git a
//! helper invocation whose only extra word is the *path* of an owner-only secret file inside the
//! owner-only run directory. The trade-off is stated in `design.md`: the secret exists briefly
//! as a `0600` file inside a `0700` directory instead of being fd-passed, because fd inheritance
//! beyond stdio is not expressible under this workspace's `unsafe` ban. The file lives entirely
//! inside Vault confinement and is deleted when the guard drops — success, timeout, overflow, or
//! cancellation alike.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Credential material for one operation.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// The identity presented to the remote.
    pub username: String,
    /// The secret presented to the remote; never logged, never passed as text.
    pub password: String,
}

impl Credentials {
    /// Every byte that must never reappear in output or arguments.
    pub(crate) fn secret_bytes(&self) -> Vec<Vec<u8>> {
        vec![
            self.username.clone().into_bytes(),
            self.password.clone().into_bytes(),
        ]
    }
}

/// Writes `credentials` into a fresh `0600` file named after a fresh UUID inside `run_dir`.
///
/// The caller must hold the returned path alive only as long as the operation and drop
/// [`SecretCleanup`] afterwards.
///
/// # Errors
///
/// Returns [`GitRunnerError::PathOutsideConfinement`] when the run directory cannot be created,
/// and [`GitRunnerError::CredentialWriteFailed`] when the file or its permissions cannot be set.
pub(crate) fn prepare_secret_file(
    run_dir: &Path,
    credentials: &Credentials,
) -> Result<PathBuf, crate::error::GitRunnerError> {
    use crate::error::GitRunnerError;

    std::fs::create_dir_all(run_dir).map_err(|source| GitRunnerError::CredentialWriteFailed {
        reason: format!("run directory {} unusable: {source}", run_dir.display()),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(run_dir)
            .map_err(|source| GitRunnerError::CredentialWriteFailed {
                reason: format!("run directory {} unreadable: {source}", run_dir.display()),
            })?
            .permissions()
            .mode();
        if dir_mode & 0o077 != 0 {
            return Err(GitRunnerError::CredentialWriteFailed {
                reason: format!(
                    "run directory {} is group/world accessible",
                    run_dir.display()
                ),
            });
        }
    }

    let path = run_dir.join(format!("cred-{}.secret", Uuid::now_v7()));
    let payload = format!(
        "username={}\npassword={}\n",
        credentials.username, credentials.password
    );
    std::fs::write(&path, payload).map_err(|source| GitRunnerError::CredentialWriteFailed {
        reason: format!("secret file {} unwritable: {source}", path.display()),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| GitRunnerError::CredentialWriteFailed {
                reason: format!("secret file {} unprotectable: {source}", path.display()),
            },
        )?;
    }

    Ok(path)
}

/// Deletes the secret file when dropped. Best effort: a leaked file inside a `0700` directory
/// still admits no other user, and the next run's cleanup pass removes strays.
pub(crate) struct SecretCleanup(pub(crate) Option<PathBuf>);

impl Drop for SecretCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ignored = std::fs::remove_file(path);
        }
    }
}
