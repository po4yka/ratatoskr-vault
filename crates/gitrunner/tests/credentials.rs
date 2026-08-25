//! The credential-helper contract, proven end to end.
//!
//! The shipped helper is exercised exactly as Git would exercise it: secret file path as the
//! single argument, the operation URL on standard input terminated by a blank line, and one
//! `username=` plus one `password=` line expected back — nothing else.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod support;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ratatoskr_vault_gitrunner::{
    Credentials, GitOperation, GitRunner, RunConfig, SourceUrl, Subcommand,
};

#[test]
fn helper_answers_credential_protocol_from_secret_file() {
    let root = support::TempRoot::new("cred");
    let secret_file = root.path().join("secret");
    std::fs::write(&secret_file, "username=u\npassword=s\n").expect("secret must be writable");
    std::fs::set_permissions(&secret_file, std::fs::Permissions::from_mode(0o600))
        .expect("permissions must be set");

    let mut child = Command::new(env!("CARGO_BIN_EXE_git-credential-helper"))
        .arg(&secret_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the helper binary must spawn");

    let mut stdin = child.stdin.take().expect("helper stdin must be piped");
    stdin
        .write_all(b"url=https://example.com/repo.git\n\n")
        .expect("the request must be writable");
    drop(stdin);

    let output = child.wait_with_output().expect("the helper must run");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "username=u\npassword=s\n",
        "the helper must answer with exactly one username line and one password line"
    );
}

#[tokio::test]
async fn secret_material_absent_from_argv_and_environment() {
    let root = support::TempRoot::new("cred-struct");
    support::instruct_probe(root.path(), "mode: argv-env\n");

    let username = "vault-operator";
    let password = "s3cr3t-P4ss-with spaces-and-\u{00e9}";
    let source = SourceUrl::parse("https://example.com/repo.git").expect("fixture URL must parse");
    let operation = GitOperation::fetch_all(&source).with_credentials(Credentials {
        username: username.to_owned(),
        password: password.to_owned(),
    });

    let runner = GitRunner::new(RunConfig {
        git_binary: support::probe_exe(),
        allowed: vec![Subcommand::Fetch],
        working_directory: root.path().to_path_buf(),
        run_home: root.path().join("home"),
        deadline: std::time::Duration::from_secs(10),
        stdout_cap_bytes: 256 * 1024,
        stderr_cap_bytes: 128 * 1024,
        credential_helper: PathBuf::from("/opt/vault/bin/git-credential-helper"),
    });

    // Structural half: the audit preview must wire the helper through ARGV words, never through
    // the environment, and the placeholder - not the secret - stands in for the file.
    let words: Vec<String> = runner
        .command_preview(&operation)
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let joined = words.join("\u{1f}");
    assert!(
        joined.contains("credential.helper="),
        "credential words missing from the command vector: {words:?}"
    );
    assert!(
        joined.contains("<staged-secret>"),
        "the staged-secret placeholder is missing from the command vector: {words:?}"
    );

    // Behavioral half: the child sees neither credential byte anywhere it could look.
    let outcome = runner
        .run(
            &GitOperation::fetch_all(&source).with_credentials(Credentials {
                username: username.to_owned(),
                password: password.to_owned(),
            }),
        )
        .await
        .expect("the probe must run");
    let seen = String::from_utf8_lossy(&outcome.stdout);
    assert!(
        !seen.contains(username) && !seen.contains(password),
        "credential material reached the child process:\n{seen}"
    );
}

#[tokio::test]
async fn leaked_secret_is_redacted_from_output() {
    let root = support::TempRoot::new("cred-redact");
    let username = "vault-operator";
    let password = "leaked-P4ss-\u{00e9}cho";
    support::instruct_probe(
        root.path(),
        &format!("mode: emit\nstream: both\ntext: prefix {username} middle {password} suffix\n"),
    );

    let source = SourceUrl::parse("https://example.com/repo.git").expect("fixture URL must parse");
    let runner = GitRunner::new(RunConfig {
        git_binary: support::probe_exe(),
        allowed: vec![Subcommand::Fetch],
        working_directory: root.path().to_path_buf(),
        run_home: root.path().join("home"),
        deadline: std::time::Duration::from_secs(10),
        stdout_cap_bytes: 256 * 1024,
        stderr_cap_bytes: 128 * 1024,
        credential_helper: root.path().join("no-helper"),
    });

    let outcome = runner
        .run(
            &GitOperation::fetch_all(&source).with_credentials(Credentials {
                username: username.to_owned(),
                password: password.to_owned(),
            }),
        )
        .await
        .expect("the echo run must complete");

    for (name, stream) in [("stdout", &outcome.stdout), ("stderr", &outcome.stderr)] {
        let text = String::from_utf8_lossy(stream);
        assert!(
            text.contains("[redacted]"),
            "{name} lost the redaction placeholder: {text}"
        );
        assert!(
            !text.contains(username) && !text.contains(password),
            "{name} still carries raw credential material: {text}"
        );
    }
}
