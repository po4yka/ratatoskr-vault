//! Typed Git bundle command tests.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::PathBuf;

use ratatoskr_vault_gitrunner::{ConfinedPath, GitOperation, GitRunner, RunConfig, Subcommand};

mod support;

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

#[tokio::test]
async fn restore_runner_refuses_live_mirror_paths_before_spawn() {
    let scratch = support::TempRoot::new("restore-denied");
    let live_mirror = scratch.path().join("live-mirrors");
    std::fs::create_dir_all(&live_mirror).expect("live mirror root must exist");
    let bundle_path = live_mirror.join("snapshot.bundle");
    std::fs::write(&bundle_path, b"immutable-live-evidence")
        .expect("live mirror sentinel must be writable");
    let before = std::fs::read(&bundle_path).expect("live mirror sentinel must be readable");
    let bundle = ConfinedPath::new(&live_mirror, PathBuf::from("snapshot.bundle").as_path())
        .expect("fixture bundle must be confined to its own root");
    support::instruct_probe(scratch.path(), "mode: mark\n");
    let runner = GitRunner::new(RunConfig {
        git_binary: support::probe_exe(),
        allowed: vec![Subcommand::Bundle],
        working_directory: scratch.path().to_path_buf(),
        run_home: scratch.path().join("home"),
        deadline: std::time::Duration::from_secs(1),
        stdout_cap_bytes: 1_024,
        stderr_cap_bytes: 1_024,
        credential_helper: scratch.path().join("no-helper"),
    })
    .with_denied_roots(vec![live_mirror]);

    let error = runner
        .run(&GitOperation::bundle_verify(&bundle))
        .await
        .expect_err("live mirror operand must be refused before spawn");

    assert!(matches!(
        error,
        ratatoskr_vault_gitrunner::GitRunnerError::PathOutsideConfinement { .. }
    ));
    assert!(!scratch.path().join("probe-marker").exists());
    assert_eq!(
        std::fs::read(bundle_path).expect("live mirror sentinel must remain readable"),
        before
    );
}

#[test]
fn restore_operations_accept_only_confined_local_bundle_paths() {
    let root = temporary_root();
    let bundle = ConfinedPath::new(&root, PathBuf::from("stored/snapshot.bundle").as_path())
        .expect("bundle path must remain confined");
    let runner = GitRunner::new(RunConfig {
        git_binary: PathBuf::from("/usr/bin/git"),
        allowed: vec![Subcommand::Init, Subcommand::Bundle, Subcommand::Fetch],
        working_directory: root.clone(),
        run_home: root.join("home"),
        deadline: std::time::Duration::from_secs(1),
        stdout_cap_bytes: 1_024,
        stderr_cap_bytes: 1_024,
        credential_helper: PathBuf::from("/usr/bin/false"),
    });

    let init = runner.command_preview(&GitOperation::init_bare());
    let verify = runner.command_preview(&GitOperation::bundle_verify(&bundle));
    let fetch = runner.command_preview(&GitOperation::fetch_bundle(&bundle));
    let environment = ratatoskr_vault_gitrunner::env::child_environment(
        PathBuf::from("/usr/bin").as_path(),
        root.join("home").as_path(),
    );

    assert_eq!(&init[2..], ["init", "--bare"]);
    assert_eq!(
        &verify[2..],
        [
            "bundle",
            "verify",
            bundle.as_path().to_str().expect("UTF-8 fixture path")
        ]
    );
    assert_eq!(
        &fetch[2..],
        [
            "fetch",
            bundle.as_path().to_str().expect("UTF-8 fixture path"),
            "+refs/*:refs/*"
        ]
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("GIT_ALLOW_PROTOCOL")),
        Some(&std::ffi::OsString::from("file"))
    );
    assert!(init.iter().chain(&verify).chain(&fetch).all(|word| {
        let word = word.to_string_lossy();
        !word.contains("://") && !word.contains('`') && !word.contains("$(")
    }));
}

fn temporary_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ratatoskr-vault-bundle-{}", std::process::id()));
    std::fs::create_dir_all(root.join("staging")).expect("test root must be creatable");
    root
}
