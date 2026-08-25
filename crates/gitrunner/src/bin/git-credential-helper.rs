//! The Git credential-helper endpoint.
//!
//! Git word-splits the `credential.helper` value and execvp's it directly (a shell is involved
//! only for values starting with `!`, which Vault never produces). The helper receives the
//! operation URL on standard input, reads its secret file — an owner-only file inside an
//! owner-only run directory whose path arrives as a plain argument, because paths are not
//! secrets — and answers with exactly one `username=` line and one `password=` line.

use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(secret_path) = std::env::args_os().nth(1) else {
        eprintln!("git-credential-helper: missing secret file argument");
        return ExitCode::FAILURE;
    };

    // Consume Git's request to EOF (Git closes the pipe after the blank line); the content
    // itself does not change the answer because the caller already bound this helper to one
    // specific operation.
    let mut request = Vec::new();
    if std::io::stdin().read_to_end(&mut request).is_err() {
        eprintln!("git-credential-helper: cannot read the credential request");
        return ExitCode::FAILURE;
    }
    let _ = request;

    let Ok(secret_text) = std::fs::read_to_string(secret_path) else {
        eprintln!("git-credential-helper: cannot read the secret file");
        return ExitCode::FAILURE;
    };

    let mut username = String::new();
    let mut password = String::new();
    for line in secret_text.lines() {
        if let Some(value) = line.strip_prefix("username=") {
            username = value.to_string();
        } else if let Some(value) = line.strip_prefix("password=") {
            password = value.to_string();
        }
    }

    let mut stdout = std::io::stdout().lock();
    let _ignored = writeln!(stdout, "username={username}");
    let _ignored = writeln!(stdout, "password={password}");
    ExitCode::SUCCESS
}
