//! Shared helpers for the runner integration tests.
//!
//! The probe executable (this crate's `tests/probe.rs`, compiled `harness = false`) stands in
//! for the trusted Git binary: `RunConfig.git_binary` is trusted configuration, so tests may
//! point it at the probe to observe exactly what a confined child would see and produce.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatoskr_vault_gitrunner::{GitRunner, RunConfig, Subcommand};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique scratch directory that removes itself on drop.
pub(crate) struct TempRoot(PathBuf);

impl TempRoot {
    /// Creates the scratch root.
    pub(crate) fn new(label: &str) -> Self {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vault-gitrunner-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch root must be created");
        Self(dir)
    }

    /// The scratch path.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

// Fixture builders are shared across test binaries; any single binary uses only part of the
// set, so the unused ones are expected, not accidental.
#[allow(dead_code)]
pub(crate) mod fixtures;

/// Locates the compiled probe executable next to this test binary.
///
/// Cargo names test binaries `<name>-<hash>` inside one directory, so the first regular file
/// whose name starts with `probe-` is the probe.
pub(crate) fn probe_exe() -> PathBuf {
    let self_exe = std::env::current_exe().expect("current test binary must be locatable");
    let dir = self_exe
        .parent()
        .expect("test binary has a parent directory");
    let entries = std::fs::read_dir(dir).expect("the test binary directory must be readable");
    let mut newest = None;
    for entry in entries.flatten() {
        let path = entry.path();
        // Cargo writes `probe-<hash>` for the binary next to `probe-<hash>.d` dep-info files
        // and object files; only the extensionless regular file is executable.
        let is_extensionless_executable = entry.file_name().to_string_lossy().starts_with("probe-")
            && path.extension().is_none()
            && path.is_file();
        if is_extensionless_executable {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if newest
                .as_ref()
                .is_none_or(|(newest_modified, _)| modified > *newest_modified)
            {
                newest = Some((modified, path));
            }
        }
    }
    if let Some((_modified, path)) = newest {
        return path;
    }
    panic!(
        "probe-<hash> executable not found next to {}",
        self_exe.display()
    );
}

/// Writes the probe instruction file into `dir`.
#[allow(dead_code, reason = "not every test binary drives the probe")]
pub(crate) fn instruct_probe(dir: &Path, instructions: &str) {
    std::fs::write(dir.join("probe-instructions"), instructions)
        .expect("probe instructions must be writable");
}

/// Builds a runner whose "Git" is the probe, executing in `working_directory`.
#[allow(dead_code, reason = "not every test binary drives the probe")]
pub(crate) fn probe_runner(
    working_directory: &Path,
    deadline: std::time::Duration,
    stdout_cap_bytes: usize,
    stderr_cap_bytes: usize,
) -> GitRunner {
    GitRunner::new(RunConfig {
        git_binary: probe_exe(),
        allowed: vec![Subcommand::Version],
        working_directory: working_directory.to_path_buf(),
        run_home: working_directory.join("home"),
        deadline,
        stdout_cap_bytes,
        stderr_cap_bytes,
        credential_helper: working_directory.join("no-helper"),
    })
}
