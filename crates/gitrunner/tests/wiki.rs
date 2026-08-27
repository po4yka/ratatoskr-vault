//! Typed wiki sibling discovery classification tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod support;

use std::path::{Path, PathBuf};

use ratatoskr_vault_gitrunner::{GitRunner, RunConfig, SourceUrl, Subcommand, WikiProbeResult};

fn probe_case(
    label: &str,
    instructions: &str,
    deadline_ms: u64,
) -> (support::TempRoot, GitRunner, SourceUrl) {
    let root = support::TempRoot::new(label);
    support::instruct_probe(root.path(), instructions);
    let parent_path = root.path().join("repository.git");
    let source = SourceUrl::parse(&format!("file://{}", parent_path.display()))
        .expect("local parent URL must validate");
    let runner = GitRunner::new(RunConfig {
        git_binary: support::probe_exe(),
        allowed: vec![Subcommand::LsRemote],
        working_directory: root.path().to_path_buf(),
        run_home: root.path().join("home"),
        deadline: std::time::Duration::from_millis(deadline_ms),
        stdout_cap_bytes: 16 * 1024,
        stderr_cap_bytes: 16 * 1024,
        credential_helper: PathBuf::from("/usr/bin/false"),
    });
    (root, runner, source)
}

#[tokio::test]
async fn wiki_probe_distinguishes_present_absent_auth_timeout_and_other_failure() {
    let (_present_root, present_runner, present_source) = probe_case(
        "wiki-present",
        "mode: emit\nstream: stdout\ntext: 0123456789012345678901234567890123456789 refs/heads/main\n",
        500,
    );
    assert_eq!(
        present_runner.probe_wiki(&present_source, true).await,
        WikiProbeResult::Present { ref_count: 1 }
    );

    let (_absent_root, absent_runner, absent_source) = probe_case(
        "wiki-absent",
        "mode: emit\nstream: stderr\ntext: does not appear to be a git repository\nexit_code: 128\n",
        500,
    );
    assert_eq!(
        absent_runner.probe_wiki(&absent_source, true).await,
        WikiProbeResult::Absent
    );

    let (_auth_root, auth_runner, auth_source) = probe_case(
        "wiki-auth",
        "mode: emit\nstream: stderr\ntext: Authentication failed\nexit_code: 128\n",
        500,
    );
    assert!(matches!(
        auth_runner.probe_wiki(&auth_source, true).await,
        WikiProbeResult::AuthenticationFailed { .. }
    ));

    let (_timeout_root, timeout_runner, timeout_source) =
        probe_case("wiki-timeout", "mode: hang\n", 50);
    assert_eq!(
        timeout_runner.probe_wiki(&timeout_source, true).await,
        WikiProbeResult::TimedOut
    );

    let (_failure_root, failure_runner, failure_source) = probe_case(
        "wiki-failure",
        "mode: emit\nstream: stderr\ntext: transport exploded\nexit_code: 2\n",
        500,
    );
    assert!(matches!(
        failure_runner.probe_wiki(&failure_source, true).await,
        WikiProbeResult::OtherFailure { .. }
    ));

    assert!(
        !Path::new("repository.wiki").exists(),
        "probe created a checkout"
    );
}
