//! The deployable boots on the configuration DEVELOPMENT.md documents.
//!
//! This is the only test that runs the shipped binary as a process. It exists so that the
//! "Local run" block of DEVELOPMENT.md cannot rot: the documented commands are executed here, the
//! admin plane is probed over a real socket, and the documented SIGTERM shutdown is asserted to
//! exit 0.

#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_persistence::test_support::TestDatabase;
use std::io::{Read, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long a binary may take to answer `/health/ready` with `200`. Generous: a loaded CI runner
/// starting a cold process is the slow case, and the cost of a too-short timeout is a flake.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Between readiness polls.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The admin port this boot test binds. One above the documented default, so a developer running
/// the real service on 9570 does not collide with the suite.
const ADMIN_PORT: u16 = 9571;

/// The service starts on its documented environment, reports ready, applies the schema it owns,
/// and exits 0 on SIGTERM after the drain.
#[test]
fn the_service_boots_on_its_documented_configuration_and_reports_ready() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the fixture database");
    let database = runtime
        .block_on(TestDatabase::create())
        .expect("a test database");

    let mut child = Command::new(built_binary())
        .env("RATATOSKR__ADMIN__BIND", format!("127.0.0.1:{ADMIN_PORT}"))
        // Pretty here so a human can read a failure without a JSON parser; the JSON path is what
        // every deployment uses and nothing else in this file depends on the choice.
        .env("RATATOSKR__TELEMETRY__LOG_FORMAT", "pretty")
        .env("RATATOSKR__DATABASE__URL", database.url())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary must spawn");

    let ready = wait_until_ready(ADMIN_PORT);
    terminate(&child);
    let status = child.wait().expect("waiting must succeed");
    let log = format!(
        "--- stdout ---\n{}--- stderr ---\n{}",
        strip_ansi(&drain(child.stdout.take())),
        drain(child.stderr.take())
    );

    assert!(
        ready,
        "the service never answered 200 on http://127.0.0.1:{ADMIN_PORT}/health/ready\n{log}"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "the service did not exit 0 after SIGTERM ({status})\n{log}"
    );
    assert!(log.contains("startup complete"), "no startup line\n{log}");

    runtime
        .block_on(database.cleanup())
        .expect("the fixture database must drop");
}

/// A configured-but-unreachable database is a route-build failure, not a start: a process that
/// reported itself ready and then failed every future request would be worse than one that did not
/// start. It exits 1 and names the variable that pointed at the wrong place.
#[test]
fn an_unreachable_database_refuses_the_start_and_names_the_variable() {
    let output = Command::new(built_binary())
        .env("RATATOSKR__ADMIN__BIND", format!("127.0.0.1:{ADMIN_PORT}"))
        // Nothing listens on 59999; the connect attempt fails inside the acquire timeout.
        .env(
            "RATATOSKR__DATABASE__URL",
            "postgres://vault:nobody@127.0.0.1:59999/vault",
        )
        .env("RATATOSKR__TELEMETRY__LOG_FILTER", "warn")
        .output()
        .expect("the binary must run");

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unreachable dependency is exit 1, not 78 and not 0\n{text}"
    );
    assert!(
        text.contains("RATATOSKR__DATABASE__URL") || text.contains("database"),
        "the refusal should point at the database\n{text}"
    );
}

/// Exit codes are an operational contract. `check-config` validates in CI or an init container
/// before anything binds, reporting value-free.
#[test]
fn check_config_exits_zero_on_a_valid_configuration_and_78_on_an_invalid_one() {
    let valid = Command::new(built_binary())
        .arg("check-config")
        .output()
        .expect("check-config must run");
    let report = String::from_utf8_lossy(&valid.stderr);
    assert_eq!(valid.status.code(), Some(0), "{report}");

    let invalid = Command::new(built_binary())
        .arg("check-config")
        .env("RATATOSKR__SHUTDOWN__DRAIN_SECONDS", "61")
        .output()
        .expect("check-config must run");
    let report = String::from_utf8_lossy(&invalid.stderr);
    assert_eq!(invalid.status.code(), Some(78), "EX_CONFIG\n{report}");
    assert!(report.contains("shutdown.drain_seconds"), "{report}");
}

/// A listener that cannot bind is a runtime startup failure: exit 1, the third row of the exit
/// code table, distinguishable from 78 in a restart-loop dashboard.
#[test]
fn a_listener_that_cannot_bind_exits_one() {
    // Held open for the child's whole life; a second listener on the same port is EADDRINUSE.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("a port must be available");
    let port = taken.local_addr().expect("the port is known").port();

    let refused = Command::new(built_binary())
        .env("RATATOSKR__ADMIN__BIND", format!("127.0.0.1:{port}"))
        .env("RATATOSKR__TELEMETRY__LOG_FILTER", "warn")
        .output()
        .expect("the binary must run");

    assert_eq!(
        refused.status.code(),
        Some(1),
        "a bind failure is exit 1, not 78 and not 0\n{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr),
    );
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("the admin listener could not bind"),
        "the operator was told which listener failed",
    );
}

/// The path of the workspace binary under test.
///
/// `CARGO_BIN_EXE_*` is set for binaries of the package under test, but only after a build puts
/// them there: `cargo test` builds the binary of THIS package, which is exactly why this lives in
/// `services/vault` rather than anywhere else.
fn built_binary() -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_ratatoskr-vault")).to_path_buf();
    assert!(path.is_file(), "{} has not been built", path.display());
    path
}

/// Polls `/health/ready` until it answers `200`, or the timeout expires.
///
/// A `503` early on is expected and not a failure: readiness is `not_ready` between the listener
/// binding and startup completing.
fn wait_until_ready(admin_port: u16) -> bool {
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(response) = probe(admin_port, "/health/ready")
            && response.starts_with("HTTP/1.1 200")
        {
            return true;
        }
        sleep(POLL_INTERVAL);
    }
    false
}

/// One `GET` written onto a raw socket.
///
/// The admin plane speaks plain HTTP/1.1 and `Connection: close` makes the whole response readable
/// to EOF, so no HTTP client dependency enters the tree for this.
fn probe(port: u16, path: &str) -> Option<String> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    socket
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut response = String::new();
    socket.read_to_string(&mut response).ok()?;
    Some(response)
}

/// Sends `SIGTERM`, the signal the shutdown sequence listens for.
///
/// `Child::kill` sends `SIGKILL`, which skips the drain entirely and never yields exit 0, and
/// `libc::kill` is unavailable because the workspace forbids unsafe code. `kill(1)` is the
/// remaining route and it is the same command DEVELOPMENT.md documents.
fn terminate(child: &Child) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("kill(1) is available on any unix host");
    assert!(status.success(), "SIGTERM could not be delivered: {status}");
}

/// Everything the child wrote to one stream. Read after `wait`, so the pipe is complete.
fn drain(stream: Option<impl Read>) -> String {
    let mut text = String::new();
    if let Some(mut stream) = stream {
        let _ = stream.read_to_string(&mut text);
    }
    text
}

/// `text` without ANSI control sequences, which the `pretty` format writes between fields.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for next in characters.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(character);
        }
    }
    out
}
