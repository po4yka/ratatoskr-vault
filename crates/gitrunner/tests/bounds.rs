//! Environment hardening and resource bounds, observed through the probe executable.
//!
//! The runner offers no channel to inject environment into a child, and the workspace forbids
//! `unsafe`, so the hostile-variable property is proven by exhaustiveness: the probe prints its
//! whole environment, and every printed key must belong to the documented allowlist while the
//! hardening guards must be present. Any inherited variable breaks the closed set immediately.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod support;

use ratatoskr_vault_gitrunner::GitOperation;

/// Every environment key a confined child may observe. Anything else is leakage.
const CHILD_ENV_ALLOWLIST: [&str; 12] = [
    "PATH",
    "HOME",
    "LANG",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PAGER",
    "GIT_ALLOW_PROTOCOL",
    "PAGER",
];

#[tokio::test]
async fn inherited_variable_never_reaches_child() {
    let root = support::TempRoot::new("env");
    support::instruct_probe(root.path(), "mode: env\n");
    let runner = support::probe_runner(
        root.path(),
        std::time::Duration::from_secs(10),
        256 * 1024,
        128 * 1024,
    );

    let outcome = runner
        .run(&GitOperation::version())
        .await
        .expect("the probe must run successfully");
    let text = String::from_utf8_lossy(&outcome.stdout);

    let mut observed_keys = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((key, _value)) = line.split_once('=') else {
            panic!("probe printed a malformed environment line: {line:?}");
        };
        assert!(
            CHILD_ENV_ALLOWLIST.contains(&key),
            "variable `{key}` reached the child but is not on the allowlist\nfull env:\n{text}"
        );
        observed_keys.push(key);
    }

    for guard in [
        "GIT_CONFIG_NOSYSTEM=1",
        "GIT_TERMINAL_PROMPT=0",
        "GIT_ALLOW_PROTOCOL=file",
    ] {
        assert!(
            text.contains(guard),
            "hardening guard {guard} missing from child env:\n{text}"
        );
    }
    assert!(
        observed_keys.len() >= CHILD_ENV_ALLOWLIST.len() - 2,
        "the child environment looks truncated rather than constructed:\n{text}"
    );
}

#[tokio::test]
async fn hung_child_is_killed_at_deadline() {
    let root = support::TempRoot::new("hang");
    support::instruct_probe(root.path(), "mode: hang\n");
    let deadline = std::time::Duration::from_millis(500);
    let runner = support::probe_runner(root.path(), deadline, 256 * 1024, 128 * 1024);

    let started = std::time::Instant::now();
    // Watchdog: bounds the whole test even while the runner ignores its own deadline (RED).
    let raced = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        runner.run(&GitOperation::version()),
    )
    .await;
    assert!(
        raced.is_ok(),
        "runner did not enforce its {}s deadline; the test watchdog had to kill the call after {:?}",
        deadline.as_secs_f32(),
        started.elapsed()
    );
    match raced.expect("watchdog guarantees presence") {
        Ok(outcome) => {
            panic!("a hung child must not complete: {outcome:?}");
        }
        Err(ratatoskr_vault_gitrunner::GitRunnerError::Timeout { .. }) => {
            let elapsed = started.elapsed();
            assert!(
                elapsed >= deadline,
                "timeout must not fire before the deadline elapsed ({elapsed:?})"
            );
            assert!(
                elapsed < std::time::Duration::from_secs(3),
                "runner returned only when the test watchdog forced it ({elapsed:?})"
            );
        }
        Err(other) => {
            panic!("expected GitRunnerError::Timeout, got {other:?}");
        }
    }

    // The child must be gone, not merely detached: the probe recorded its pid before hanging.
    // `kill -0` exits 0 while the process exists and fails once it is reaped.
    let pid_file = root.path().join("probe-pid");
    let pid_text =
        std::fs::read_to_string(&pid_file).expect("hang probe must have written its pid");
    let pid = pid_text.trim();
    let liveness = std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg(pid)
        .output()
        .expect("kill -0 must be runnable");
    assert!(
        !liveness.status.success(),
        "child process {pid} still exists after the runner killed it"
    );
}

async fn overflow_case(stream: &str, cap: usize) {
    let root = support::TempRoot::new("overflow");
    support::instruct_probe(
        root.path(),
        &format!("mode: emit\nstream: {stream}\nbytes: {}\n", cap + 1024),
    );
    let (out_cap, err_cap) = match stream {
        "stdout" => (cap, 128 * 1024),
        _ => (128 * 1024, cap),
    };
    let runner = support::probe_runner(
        root.path(),
        std::time::Duration::from_secs(10),
        out_cap,
        err_cap,
    );

    let started = std::time::Instant::now();
    let error = runner
        .run(&GitOperation::version())
        .await
        .expect_err("a stream past its cap must fail the run");
    let elapsed = started.elapsed();

    let (is_stdout, retained) = match &error {
        ratatoskr_vault_gitrunner::GitRunnerError::OutputLimitExceeded { stream, retained } => (
            *stream == ratatoskr_vault_gitrunner::outcome::Stream::Stdout,
            *retained,
        ),
        other => panic!("expected OutputLimitExceeded for the {stream} stream, got {other:?}"),
    };
    assert_eq!(
        is_stdout,
        stream == "stdout",
        "the wrong stream was blamed: {error}"
    );
    assert!(
        retained <= cap,
        "retained {retained} bytes exceeds the {cap}-byte cap"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "overflow handling must terminate the run promptly, took {elapsed:?}"
    );
}

#[tokio::test]
async fn oversized_stdout_terminates_child_and_reports_stream() {
    overflow_case("stdout", 4096).await;
}

#[tokio::test]
async fn oversized_stderr_terminates_child_and_reports_stream() {
    overflow_case("stderr", 4096).await;
}

#[tokio::test]
async fn clean_completion_reports_structured_evidence() {
    let root = support::TempRoot::new("clean");
    support::instruct_probe(root.path(), "mode: emit\nstream: stdout\nbytes: 1024\n");
    let runner = support::probe_runner(
        root.path(),
        std::time::Duration::from_secs(10),
        256 * 1024,
        128 * 1024,
    );

    let outcome = runner
        .run(&GitOperation::version())
        .await
        .expect("a clean run must succeed");

    assert_eq!(outcome.exit_code, 0, "probe exit code must be reported");
    assert_eq!(outcome.stdout.len(), 1024, "stdout must be captured");
    assert!(
        !outcome.stdout_truncated && !outcome.stderr_truncated,
        "no truncation may be flagged on a clean run"
    );
    assert!(
        !outcome.duration.is_zero(),
        "the measured duration must not be the zero placeholder"
    );
}
