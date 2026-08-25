//! The hostile-repository suite: generated fixtures attacking the runner, each proving one
//! containment property from `openspec/specs/git-execution`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod support;

use std::path::PathBuf;

use ratatoskr_vault_gitrunner::{GitOperation, GitRunner, RunConfig, SourceUrl, Subcommand};

fn runner_for(working_directory: PathBuf, allowed: &[Subcommand]) -> GitRunner {
    let root = working_directory
        .parent()
        .expect("working directory has a parent")
        .to_path_buf();
    GitRunner::new(RunConfig {
        git_binary: support::fixtures::system_git(),
        allowed: allowed.to_vec(),
        working_directory,
        run_home: root.join("home"),
        deadline: std::time::Duration::from_mins(1),
        stdout_cap_bytes: 256 * 1024,
        stderr_cap_bytes: 128 * 1024,
        credential_helper: PathBuf::from("/usr/bin/true"),
    })
}

/// Reads `git rev-list --all` output from `repo_dir` through the runner.
async fn rev_list_all(repo_dir: PathBuf) -> String {
    let runner = runner_for(repo_dir, &[Subcommand::RevList]);
    let outcome = runner
        .run(&GitOperation::rev_list_all())
        .await
        .expect("rev-list must run");
    assert_eq!(outcome.exit_code, 0, "rev-list failed");
    String::from_utf8_lossy(&outcome.stdout).into_owned()
}

/// Reads the full object inventory (`--objects --all`) from `repo_dir`, sorted.
async fn rev_list_all_objects(repo_dir: PathBuf) -> Vec<String> {
    let runner = runner_for(repo_dir, &[Subcommand::RevList]);
    let outcome = runner
        .run(&GitOperation::rev_list_all_objects())
        .await
        .expect("rev-list --objects must run");
    assert_eq!(outcome.exit_code, 0, "rev-list --objects failed");
    let mut lines: Vec<String> = String::from_utf8_lossy(&outcome.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

#[tokio::test]
async fn generator_reproduces_identical_commit_ids() {
    let first = support::TempRoot::new("determinism-a");
    let second = support::TempRoot::new("determinism-b");

    let one = support::fixtures::deterministic_history(first.path());
    let two = support::fixtures::deterministic_history(second.path());

    let oids_one = rev_list_all(one.source.clone()).await;
    let oids_two = rev_list_all(two.source.clone()).await;

    assert!(
        !oids_one.is_empty(),
        "the fixture history must contain commits"
    );
    assert_eq!(
        oids_one, oids_two,
        "two builds of the same fixture produced different object ids"
    );
}

#[tokio::test]
async fn hostile_ref_names_survive_mirror_as_data() {
    use ratatoskr_vault_gitrunner::confine::TargetIdentity;

    let root = support::TempRoot::new("refnames");
    std::fs::create_dir_all(root.path().join("mirrors")).expect("mirrors dir");
    let fixture = support::fixtures::hostile_refnames(root.path());
    let destination_root = root.path().join("mirrors");

    // The mirror location derives from an internal id; the hostile ref names are content.
    let identity = TargetIdentity {
        id: uuid::Uuid::now_v7(),
        display_name: "../../definitely-not-a-path".to_owned(),
    };
    let destination = identity
        .mirror_path(&destination_root)
        .expect("layout must derive");

    let source =
        SourceUrl::parse(&format!("file://{}", fixture.source.display())).expect("fixture URL");
    let clone_runner = runner_for(destination_root.clone(), &[Subcommand::CloneMirror]);
    let cloned = clone_runner
        .run(&GitOperation::clone_mirror(&source, &destination))
        .await
        .expect("the clone must run");
    assert_eq!(
        cloned.exit_code,
        0,
        "clone failed: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );

    let list_runner = runner_for(destination.as_path().to_path_buf(), &[Subcommand::ShowRef]);
    let listed = list_runner
        .run(&GitOperation::show_ref())
        .await
        .expect("show-ref must run");
    assert_eq!(
        listed.exit_code,
        0,
        "show-ref failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    // Byte-for-byte: every created name must come back exactly.
    let mut expected: Vec<String> = Vec::new();
    for name in &fixture.names {
        expected.push(format!("refs/heads/{name}"));
        if name != "plain-main" && !name.starts_with("back") {
            expected.push(format!("refs/tags/tag-{name}"));
        }
    }
    expected.sort();

    let mut observed: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(_, reference)| reference.to_owned())
        })
        .collect();
    observed.sort();

    assert_eq!(
        observed.len(),
        expected.len(),
        "ref count drifted: {observed:?}"
    );
    for (got, want) in observed.iter().zip(expected.iter()) {
        assert_eq!(
            got, want,
            "a hostile ref name did not survive byte-for-byte"
        );
    }

    // Nothing named by the embedded marker token may exist anywhere under the storage root.
    let mut stack = vec![root.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("directory must be readable") {
            let path = entry.expect("entry must be readable").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                assert_ne!(
                    path.file_name().and_then(|n| n.to_str()),
                    Some("pwned-marker"),
                    "shell metacharacters in a ref name executed as a command"
                );
            }
        }
    }
}

#[tokio::test]
async fn large_object_repository_verifies_within_bounds() {
    use ratatoskr_vault_gitrunner::SourceUrl;
    use ratatoskr_vault_gitrunner::confine::TargetIdentity;

    let root = support::TempRoot::new("large-blob");
    let mirrors = root.path().join("mirrors");
    std::fs::create_dir_all(&mirrors).expect("mirrors dir");
    let fixture = support::fixtures::large_blob_repo(root.path());

    let identity = TargetIdentity {
        id: uuid::Uuid::now_v7(),
        display_name: "irrelevant".to_owned(),
    };
    let destination = identity.mirror_path(&mirrors).expect("layout must derive");

    let source =
        SourceUrl::parse(&format!("file://{}", fixture.source.display())).expect("fixture URL");
    let clone_runner = runner_for(mirrors.clone(), &[Subcommand::CloneMirror]);
    let cloned = clone_runner
        .run(&GitOperation::clone_mirror(&source, &destination))
        .await
        .expect("the clone must run");
    assert_eq!(
        cloned.exit_code,
        0,
        "clone failed: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );

    // Integrity verification on the mirrored copy: success, inside the deadline, nothing
    // truncated.
    let fsck_runner = runner_for(destination.as_path().to_path_buf(), &[Subcommand::Fsck]);
    let verified = fsck_runner
        .run(&GitOperation::fsck_full())
        .await
        .expect("fsck must run");
    assert_eq!(
        verified.exit_code,
        0,
        "fsck failed on the large-object mirror: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        !verified.stdout_truncated && !verified.stderr_truncated,
        "a healthy large-object verification must stay within its caps"
    );
    assert!(
        verified.duration < std::time::Duration::from_mins(1),
        "verification exceeded its budget: {:?}",
        verified.duration
    );

    // The blob really made it across: the mirror's object set equals the source's, object for
    // object (oids and paths), and the physical store holds bulk bytes because the fixture
    // pattern is incompressible.
    let source_objects = rev_list_all_objects(fixture.source.clone()).await;
    let mirrored_objects = rev_list_all_objects(destination.as_path().to_path_buf()).await;
    assert_eq!(
        source_objects, mirrored_objects,
        "the mirrored object set differs from the source"
    );

    let mut objects_bytes = 0_u64;
    let objects_dir = destination.as_path().join("objects");
    let mut stack = vec![objects_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("objects dir readable") {
            let path = entry.expect("entry readable").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                objects_bytes += path.metadata().expect("object size").len();
            }
        }
    }
    assert!(
        objects_bytes >= 1024 * 1024,
        "expected bulk object data in the mirror, found {objects_bytes} bytes"
    );
}

#[tokio::test]
async fn promisor_integrity_failure_is_classified_not_hung() {
    use ratatoskr_vault_gitrunner::GitRunnerError;

    let root = support::TempRoot::new("promisor");
    let fixture = support::fixtures::promisor_mirror(root.path());

    let runner = runner_for(fixture.mirror.clone(), &[Subcommand::Fsck]);
    let started = std::time::Instant::now();
    let error = runner
        .run(&GitOperation::fsck_full())
        .await
        .expect_err("verification of a mirror with missing objects must fail");
    let elapsed = started.elapsed();

    let GitRunnerError::IntegrityCheckFailed { tool, excerpt } = &error else {
        panic!("expected IntegrityCheckFailed, got {error:?}");
    };
    assert_eq!(*tool, "fsck", "the failing tool must be named");
    assert!(
        !excerpt.is_empty(),
        "diagnostic excerpts must survive into the typed failure"
    );
    assert!(
        excerpt.contains("invalid sha1 pointer") || excerpt.contains("missing"),
        "the excerpt should carry git's own diagnosis: {excerpt}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "integrity detection must be bounded, took {elapsed:?}"
    );
}

#[tokio::test]
async fn hooks_configured_in_mirror_never_execute() {
    let root = support::TempRoot::new("hooks");
    let fixture = support::fixtures::hooked_mirror(root.path());

    let source = SourceUrl::parse(&fixture.source_url).expect("file:// fixture must validate");
    let runner = runner_for(fixture.mirror.clone(), &[Subcommand::Fetch]);

    let outcome = runner
        .run(&GitOperation::fetch_all(&source))
        .await
        .expect("the fetch itself must succeed");
    assert_eq!(
        outcome.exit_code,
        0,
        "fetch failed: {}",
        String::from_utf8_lossy(&outcome.stderr)
    );

    // Walk the whole storage root: no hostile hook may have executed anywhere.
    let mut stack = vec![root.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("directory must be readable") {
            let path = entry.expect("entry must be readable").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                assert_ne!(
                    path.file_name(),
                    fixture.marker.file_name(),
                    "hostile hook executed and wrote its marker: {}",
                    path.display()
                );
            }
        }
    }
}
