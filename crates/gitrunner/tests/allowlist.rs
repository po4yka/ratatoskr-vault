//! The subcommand allowlist is enforced before any process exists.
//!
//! The sentinel binary is `/usr/bin/touch`: if a refused request ever spawns, `touch` leaves its
//! marker file behind and the containment assertion fails loudly.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatoskr_vault_gitrunner::{GitOperation, GitRunner, GitRunnerError, RunConfig};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique scratch directory that removes itself on drop.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vault-gitrunner-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch root must be created");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn runner_allowing_only_version(cwd: &Path) -> GitRunner {
    GitRunner::new(RunConfig {
        git_binary: PathBuf::from("/usr/bin/touch"),
        allowed: vec![ratatoskr_vault_gitrunner::Subcommand::Version],
        working_directory: cwd.to_path_buf(),
        run_home: cwd.join("home"),
        deadline: std::time::Duration::from_secs(10),
        stdout_cap_bytes: 256 * 1024,
        stderr_cap_bytes: 128 * 1024,
        credential_helper: cwd.join("no-helper"),
    })
}

#[tokio::test]
async fn disallowed_subcommand_is_refused_before_spawn() {
    let root = TempRoot::new("allowlist");
    let marker = root.path().join("version");

    let runner = runner_allowing_only_version(root.path());
    let error = runner
        .run(&GitOperation::fsck_full())
        .await
        .expect_err("a disallowed subcommand must be refused");

    assert!(
        matches!(error, GitRunnerError::DisallowedSubcommand { .. }),
        "expected DisallowedSubcommand, got {error:?}"
    );
    assert!(
        !marker.exists(),
        "the sentinel executed: a refused request must not spawn a process"
    );
}
