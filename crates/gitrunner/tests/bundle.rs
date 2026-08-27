//! Typed Git bundle command tests.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::PathBuf;

use ratatoskr_vault_gitrunner::{ConfinedPath, GitOperation, GitRunner, RunConfig, Subcommand};

#[test]
fn full_bundle_operation_is_typed_all_ref_and_confined() {
    let root = temporary_root();
    let output = ConfinedPath::new(&root, PathBuf::from("staging/snapshot.bundle").as_path())
        .expect("bundle destination must remain confined");
    let runner = GitRunner::new(RunConfig {
        git_binary: PathBuf::from("/usr/bin/git"),
        allowed: vec![Subcommand::Bundle],
        working_directory: root.clone(),
        run_home: root.clone(),
        deadline: std::time::Duration::from_secs(1),
        stdout_cap_bytes: 1_024,
        stderr_cap_bytes: 1_024,
        credential_helper: PathBuf::from("/usr/bin/false"),
    });

    let preview = runner.command_preview(&GitOperation::bundle_create(&output));

    assert_eq!(preview[2], "bundle");
    assert_eq!(preview[3], "create");
    assert_eq!(preview[4], output.as_path().as_os_str());
    assert_eq!(preview[5], "--all");
}

fn temporary_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ratatoskr-vault-bundle-{}", std::process::id()));
    std::fs::create_dir_all(root.join("staging")).expect("test root must be creatable");
    root
}
