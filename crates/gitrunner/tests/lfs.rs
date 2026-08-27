//! Git LFS argument and hostile-input containment tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod support;

use std::path::Path;

use ratatoskr_vault_gitrunner::{
    ConfinedPath, LfsOperation, LfsRunConfig, LfsRunner, LfsTransferMode,
};

#[tokio::test]
async fn lfs_arguments_and_hostile_pointer_text_never_execute_a_shell_or_filter() {
    let root = support::TempRoot::new("lfs-hostile");
    let storage = root.path().join("stage");
    let mirror = root.path().join("mirror");
    std::fs::create_dir_all(&mirror).expect("mirror fixture must exist");
    let marker = root.path().join("pointer-executed");
    let hostile = format!("$(touch {}) ; echo filter-owned", marker.display());
    support::instruct_probe(
        &mirror,
        &format!("mode: emit\nstream: stdout\ntext: {hostile}\n"),
    );

    let runner = LfsRunner::new(LfsRunConfig {
        lfs_binary: support::probe_exe(),
        mirror_directory: ConfinedPath::new(root.path(), Path::new("mirror"))
            .expect("mirror stays confined"),
        storage_directory: ConfinedPath::new(root.path(), Path::new("stage"))
            .expect("storage stays confined"),
        run_home: ConfinedPath::new(root.path(), Path::new("home")).expect("home stays confined"),
        transfer_mode: LfsTransferMode::Basic,
        deadline: std::time::Duration::from_secs(2),
        stdout_cap_bytes: 16 * 1024,
        stderr_cap_bytes: 16 * 1024,
    });
    assert_eq!(
        LfsRunner::command_preview(LfsOperation::ListAllLong),
        ["ls-files", "--all", "--long"],
        "the LFS operation must remain a fixed opaque argv vector"
    );

    let outcome = runner
        .run(LfsOperation::ListAllLong)
        .await
        .expect("the typed LFS operation must execute through the bounded runner");

    assert_eq!(String::from_utf8_lossy(&outcome.stdout), hostile);
    assert!(
        !marker.exists(),
        "hostile pointer text executed as a command"
    );
    assert!(
        !storage.exists()
            || storage
                .read_dir()
                .expect("stage is readable")
                .next()
                .is_none(),
        "enumeration wrote unexpected bytes outside its typed output"
    );
}

#[tokio::test]
async fn cancelled_lfs_fetch_kills_the_whole_process_group() {
    let root = support::TempRoot::new("lfs-cancel");
    let mirror = root.path().join("mirror");
    std::fs::create_dir_all(&mirror).expect("mirror fixture must exist");
    support::instruct_probe(&mirror, "mode: hang-with-descendant\n");
    let runner = LfsRunner::new(LfsRunConfig {
        lfs_binary: support::probe_exe(),
        mirror_directory: ConfinedPath::new(root.path(), Path::new("mirror"))
            .expect("mirror stays confined"),
        storage_directory: ConfinedPath::new(root.path(), Path::new("stage"))
            .expect("storage stays confined"),
        run_home: ConfinedPath::new(root.path(), Path::new("home")).expect("home stays confined"),
        transfer_mode: LfsTransferMode::Basic,
        deadline: std::time::Duration::from_secs(2),
        stdout_cap_bytes: 16 * 1024,
        stderr_cap_bytes: 16 * 1024,
    });
    let (cancel, mut cancellation) = tokio::sync::watch::channel(false);
    let mirror_for_signal = mirror.clone();
    let signal = tokio::spawn(async move {
        for _ in 0..100 {
            if mirror_for_signal.join("probe-descendant-pid").exists() {
                cancel
                    .send(true)
                    .expect("cancellation receiver remains alive");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("probe descendant did not start before cancellation watchdog");
    });

    let result = runner
        .run_cancellable(LfsOperation::FetchAll, &mut cancellation)
        .await;
    signal.await.expect("signal task must finish");
    assert!(
        matches!(
            result,
            Err(ratatoskr_vault_gitrunner::LfsRunnerError::Interrupted)
        ),
        "cancellation must return a typed interruption, got {result:?}"
    );

    for pid_file in ["probe-pid", "probe-descendant-pid"] {
        let pid =
            std::fs::read_to_string(mirror.join(pid_file)).expect("probe pid evidence must exist");
        let liveness = std::process::Command::new("/bin/kill")
            .arg("-0")
            .arg(pid.trim())
            .output()
            .expect("kill liveness check must run");
        assert!(
            !liveness.status.success(),
            "process {} from {pid_file} survived cancellation",
            pid.trim()
        );
    }
}
