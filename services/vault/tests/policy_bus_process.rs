//! The shipped binary with a bus configured: the lane works end to end inside the process, and
//! losing the broker flips `/health/ready` and ends the process non-zero instead of leaving a
//! ready process that can no longer do its work (XR-021 CONTRACTS.md section S02 rule 5).
//!
//! Each test runs its own broker on a private port, so the broker can be killed without touching
//! the shared development server.

#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use std::io::{Read, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ratatoskr_vault::test_support::{
    Broker, claim_port, command_bytes, provision_topology, publish_command,
    wait_for_acknowledgements,
};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

const PATIENCE: Duration = Duration::from_secs(40);

const REPO_A: &str = "repository:018f0000-0000-7000-8000-000000000501";

/// The binary under test, killed when the test ends however it ends.
struct Process {
    child: Child,
    admin_port: u16,
    log: PathBuf,
}

impl Process {
    /// Starts the binary. Its output goes to a file, never an undrained pipe: a child that logs
    /// more than a pipe holds would block on its own diagnostics.
    fn spawn(database_url: &str, broker_url: &str, directory: &Path) -> Self {
        let admin_port = claim_port().expect("a private admin port");
        let log = directory.join("vault.log");
        let output = std::fs::File::create(&log).expect("a log file");
        let errors = output.try_clone().expect("a second handle on the log");
        let child = Command::new(built_binary())
            .env("RATATOSKR__ADMIN__BIND", format!("127.0.0.1:{admin_port}"))
            .env("RATATOSKR__TELEMETRY__LOG_FORMAT", "pretty")
            .env("RATATOSKR__DATABASE__URL", database_url)
            .env("RATATOSKR__BUS__URL", broker_url)
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("the binary must spawn");
        Self {
            child,
            admin_port,
            log,
        }
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn ready(&self) -> Option<bool> {
        probe(self.admin_port, "/health/ready").map(|response| response.starts_with("HTTP/1.1 200"))
    }

    fn body(&self) -> String {
        probe(self.admin_port, "/health/ready").unwrap_or_default()
    }

    /// The first `200` readiness response, headers and body included.
    fn wait_for_ready_response(&self) -> String {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let response = probe(self.admin_port, "/health/ready").unwrap_or_default();
            if response.starts_with("HTTP/1.1 200") {
                return response;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the process never became ready");
    }

    fn wait_for_exit(&mut self) -> Option<i32> {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("waiting must work") {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn built_binary() -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_ratatoskr-vault")).to_path_buf();
    assert!(path.is_file(), "{} has not been built", path.display());
    path
}

/// One `GET` over a raw socket; `None` when nothing answers.
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

/// The process connects, verifies the Edge-provisioned durable, reports the bus as a named
/// passing check, applies a published policy and acknowledges it: the lane is wired into `main`.
#[tokio::test(flavor = "multi_thread")]
async fn the_binary_applies_a_published_policy_and_acknowledges_it() {
    let directory = tempdir("end-to-end");
    let broker = Broker::spawn(&directory, "").expect("a private broker");
    let client = async_nats::connect(broker.url()).await.expect("connect");
    let context = provision_topology(&client).await.expect("topology");
    let database = TestDatabase::create().await.expect("a database");

    let process = Process::spawn(&database.url(), &broker.url(), &directory);
    let probing = tokio::task::block_in_place(|| process.wait_for_ready_response());
    assert!(
        probing.contains("\"bus\""),
        "a ready process with a bus reports the bus as a check\n{probing}"
    );

    let command_id = Uuid::now_v7();
    let payload = command_bytes(command_id, 1, &[REPO_A]).expect("a command");
    publish_command(&context, payload, "process-test")
        .await
        .expect("published");
    let acknowledgements = wait_for_acknowledgements(&context, 1, PATIENCE)
        .await
        .expect("the process acknowledged the command");
    assert_eq!(
        acknowledgements[0].envelope.aggregate_id.to_wire(),
        "backup_policy:1"
    );

    drop(process);
    database.cleanup().await.expect("cleanup");
}

/// Killing the broker flips readiness to failed with the bus named, and the process exits
/// non-zero after its drain; it does not sit ready.
#[tokio::test(flavor = "multi_thread")]
async fn losing_the_broker_flips_readiness_and_exits_non_zero() {
    let directory = tempdir("broker-loss");
    let mut broker = Broker::spawn(&directory, "").expect("a private broker");
    let client = async_nats::connect(broker.url()).await.expect("connect");
    provision_topology(&client).await.expect("topology");
    let database = TestDatabase::create().await.expect("a database");

    let mut process = Process::spawn(&database.url(), &broker.url(), &directory);
    tokio::task::block_in_place(|| process.wait_for_ready_response());

    broker.stop();

    let went_not_ready = tokio::task::block_in_place(|| {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            // Connection refused counts too: the drain window is short and the process may
            // already be gone, which is the exit the next assertion checks.
            if process.ready() != Some(true) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    });
    assert!(
        went_not_ready,
        "readiness stayed 200 after the broker was killed\n{}\n--- process log ---\n{}",
        process.body(),
        process.log_text()
    );

    let code = tokio::task::block_in_place(|| process.wait_for_exit());
    assert!(
        matches!(code, Some(status) if status != 0),
        "the process must exit non-zero after losing the broker, got {code:?}\n{}",
        process.log_text()
    );
    database.cleanup().await.expect("cleanup");
}

/// A scratch directory under the package's `target/tmp`, which cargo provides to integration
/// tests and which stays inside the clone.
fn tempdir(label: &str) -> PathBuf {
    let path =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("bus-{label}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
