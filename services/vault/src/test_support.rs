//! Scaffolding for the bus tests, compiled only with the `test-support` feature.
//!
//! It never reaches the deployable. It builds the topology Edge provisions in production (the
//! two streams and the Vault durable of XR-021 CONTRACTS.md section S04), builds policy commands,
//! reads what the lane acknowledged, and runs a private `nats-server` for the tests that need an
//! authorization-enabled or disposable broker. Failures are plain strings: the callers are test
//! binaries that assert on them.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::File;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_nats::HeaderMap;
use async_nats::jetstream::{self, consumer, stream};
use ratatoskr_event_envelope::{CommandEnvelope, EventEnvelope};
use serde_json::json;
use uuid::Uuid;

use crate::policy_bus::{COMMAND_SUBJECT, COMMANDS_STREAM, DURABLE, EVENTS_STREAM};

/// The shared development broker the integration tests use when they do not spawn their own.
///
/// `VAULT_TEST_NATS_URL` overrides it. A missing broker is a failure, not a reason to skip.
#[must_use]
#[expect(
    clippy::disallowed_methods,
    reason = "the workspace bans direct environment reads so that configuration has exactly one \
              loader. This is test-only scaffolding that never runs in a service binary, and it \
              reads a variable that is not part of the vault configuration at all: it names where \
              the bus tests may create and delete streams."
)]
pub fn nats_url() -> String {
    std::env::var("VAULT_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned())
}

/// Deletes and recreates the two streams and the Vault durable exactly as Edge provisions them in
/// production: commands on `cmd.>`, events on `evt.>`, and a pull consumer with explicit
/// acknowledgement, a 30 second ack wait, unlimited redelivery and the policy filter.
///
/// # Errors
///
/// A description of the first `JetStream` call that failed.
pub async fn provision_topology(client: &async_nats::Client) -> Result<jetstream::Context, String> {
    let context = jetstream::new(client.clone());
    for (name, subjects) in [(COMMANDS_STREAM, "cmd.>"), (EVENTS_STREAM, "evt.>")] {
        // Absent on a fresh broker; the delete is a reset, not a requirement.
        let _ = context.delete_stream(name).await;
        context
            .create_stream(stream::Config {
                name: name.to_owned(),
                subjects: vec![subjects.to_owned()],
                storage: stream::StorageType::Memory,
                ..Default::default()
            })
            .await
            .map_err(|error| format!("stream {name} was not created: {error}"))?;
    }
    context
        .create_consumer_on_stream(
            consumer::pull::Config {
                durable_name: Some(DURABLE.to_owned()),
                filter_subject: COMMAND_SUBJECT.to_owned(),
                ack_policy: consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                max_deliver: -1,
                ..Default::default()
            },
            COMMANDS_STREAM,
        )
        .await
        .map_err(|error| format!("durable {DURABLE} was not created: {error}"))?;
    Ok(context)
}

/// The canonical bytes of a `vault.backup_policy.apply_requested.v1` command envelope, as GitHub's
/// outbox pump publishes it: producer `ratatoskr-github`, aggregate and correlation
/// `backup_policy:<version>`, no tenant, the policy naming `repositories`.
///
/// # Errors
///
/// A description of the envelope member that was rejected.
pub fn command_bytes(
    command_id: Uuid,
    policy_version: u64,
    repositories: &[&str],
) -> Result<Vec<u8>, String> {
    let entries: Vec<serde_json::Value> = repositories
        .iter()
        .map(|reference| {
            json!({
                "repository_ref": reference,
                "mirror_cadence": "daily",
                "priority_hint": "standard",
            })
        })
        .collect();
    let envelope = json!({
        "command_id": command_id.to_string(),
        "command_type": "vault.backup_policy.apply_requested.v1",
        "issued_at": "2026-08-20T09:00:00Z",
        "producer": "ratatoskr-github",
        "aggregate_id": format!("backup_policy:{policy_version}"),
        "correlation_id": format!("backup_policy:{policy_version}"),
        "schema_version": 1,
        "payload": {
            "policy": {
                "policy_version": policy_version,
                "producing_service": "ratatoskr-github",
                "produced_at": "2026-08-20T09:00:00Z",
                "repositories": entries,
            },
        },
    });
    let parsed = CommandEnvelope::from_json(envelope.to_string().as_bytes())
        .map_err(|error| format!("the command envelope is invalid: {error}"))?;
    parsed
        .to_canonical_json()
        .map(String::into_bytes)
        .map_err(|error| format!("the command envelope has no canonical form: {error}"))
}

/// Publishes `payload` on the policy command subject with `Nats-Msg-Id` set to `message_id` and
/// waits for the broker's acknowledgement. A distinct id defeats the stream's duplicate window,
/// which is how a test makes the broker redeliver the same command.
///
/// # Errors
///
/// A description of the publish or acknowledgement failure.
pub async fn publish_command(
    context: &jetstream::Context,
    payload: Vec<u8>,
    message_id: &str,
) -> Result<u64, String> {
    let mut headers = HeaderMap::new();
    headers.insert("Nats-Msg-Id", message_id);
    let ack = context
        .publish_with_headers(COMMAND_SUBJECT, headers, payload.into())
        .await
        .map_err(|error| format!("the command was not published: {error}"))?
        .await
        .map_err(|error| format!("the command was not acknowledged: {error}"))?;
    Ok(ack.sequence)
}

/// One acknowledgement as the broker stored it.
#[derive(Debug, Clone)]
pub struct StoredAcknowledgement {
    /// The decoded envelope.
    pub envelope: EventEnvelope,
    /// The `Nats-Msg-Id` header the publisher set, when it set one.
    pub message_id: Option<String>,
    /// The subject the broker stored it on.
    pub subject: String,
}

/// How many acknowledgement events the events stream holds.
///
/// # Errors
///
/// A description of the stream lookup failure.
pub async fn acknowledgement_count(context: &jetstream::Context) -> Result<u64, String> {
    let mut stream = context
        .get_stream(EVENTS_STREAM)
        .await
        .map_err(|error| format!("the events stream is missing: {error}"))?;
    let info = stream
        .info()
        .await
        .map_err(|error| format!("the events stream has no info: {error}"))?;
    Ok(info.state.messages)
}

/// Waits until the events stream holds at least `count` acknowledgements, then returns every one
/// of them in order.
///
/// # Errors
///
/// A description of what the stream held when `within` elapsed, or of a lookup failure.
pub async fn wait_for_acknowledgements(
    context: &jetstream::Context,
    count: u64,
    within: Duration,
) -> Result<Vec<StoredAcknowledgement>, String> {
    let deadline = Instant::now() + within;
    loop {
        let held = acknowledgement_count(context).await?;
        if held >= count {
            return read_acknowledgements(context, held).await;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{held} of {count} acknowledgements arrived within {within:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_acknowledgements(
    context: &jetstream::Context,
    held: u64,
) -> Result<Vec<StoredAcknowledgement>, String> {
    let stream = context
        .get_stream(EVENTS_STREAM)
        .await
        .map_err(|error| format!("the events stream is missing: {error}"))?;
    let mut stored = Vec::new();
    for sequence in 1..=held {
        let message = stream
            .get_raw_message(sequence)
            .await
            .map_err(|error| format!("message {sequence} was not readable: {error}"))?;
        let envelope = EventEnvelope::from_json(&message.payload)
            .map_err(|error| format!("message {sequence} is not an event envelope: {error}"))?;
        stored.push(StoredAcknowledgement {
            envelope,
            message_id: message
                .headers
                .get("Nats-Msg-Id")
                .map(|value| value.as_str().to_owned()),
            subject: message.subject.to_string(),
        });
    }
    Ok(stored)
}

/// Waits until the Vault durable has acknowledged every message up to stream sequence `through`
/// and holds none in flight: the lane is finished with them, whatever the disposition was.
///
/// # Errors
///
/// A description of the consumer state when `within` elapsed, or of a lookup failure.
pub async fn wait_until_settled(
    context: &jetstream::Context,
    through: u64,
    within: Duration,
) -> Result<(), String> {
    let mut durable: consumer::PullConsumer = context
        .get_consumer_from_stream(DURABLE, COMMANDS_STREAM)
        .await
        .map_err(|error| format!("the durable is missing: {error}"))?;
    let deadline = Instant::now() + within;
    loop {
        let info = durable
            .info()
            .await
            .map_err(|error| format!("the durable has no info: {error}"))?;
        if info.ack_floor.stream_sequence >= through && info.num_ack_pending == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the durable acknowledged through {} with {} in flight; wanted {through}",
                info.ack_floor.stream_sequence, info.num_ack_pending
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A `nats-server` child process on a private port, killed when dropped.
#[derive(Debug)]
pub struct Broker {
    child: Child,
    port: u16,
    log: PathBuf,
}

/// Ports this process already handed out, so two tests of one binary never pick the same one.
fn claimed_ports() -> &'static Mutex<HashSet<u16>> {
    static CLAIMED: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The inclusive range private test servers may use, from `VAULT_TEST_NATS_PORTS` (`start-end`),
/// or `None` to let the operating system choose.
#[expect(
    clippy::disallowed_methods,
    reason = "test-only scaffolding that reads a variable outside the vault configuration: it \
              names the ports a spawned test broker may take"
)]
fn configured_ports() -> Option<(u16, u16)> {
    let raw = std::env::var("VAULT_TEST_NATS_PORTS").ok()?;
    let (start, end) = raw.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

/// A loopback port nothing listens on and this process has not handed out.
///
/// # Errors
///
/// A description of why no port was available.
pub fn claim_port() -> Result<u16, String> {
    let mut claimed = claimed_ports()
        .lock()
        .map_err(|_| "the port registry is poisoned".to_owned())?;
    let candidates: Box<dyn Iterator<Item = u16>> = match configured_ports() {
        Some((start, end)) => Box::new(start..=end),
        None => Box::new(std::iter::once(0)),
    };
    for candidate in candidates {
        if candidate != 0 && claimed.contains(&candidate) {
            continue;
        }
        let Ok(listener) = TcpListener::bind(("127.0.0.1", candidate)) else {
            continue;
        };
        let port = listener
            .local_addr()
            .map_err(|error| format!("the bound port is unknown: {error}"))?
            .port();
        claimed.insert(port);
        return Ok(port);
    }
    Err("no free port in the configured range".to_owned())
}

impl Broker {
    /// Starts `nats-server` with `JetStream` on a private loopback port, with `extra` appended to
    /// its configuration (an `authorization` block, for the tests that need one), and waits until
    /// it accepts connections. Its log is kept in `directory` so a test can read what the server
    /// refused.
    ///
    /// # Errors
    ///
    /// A description of why the server did not start.
    pub fn spawn(directory: &Path, extra: &str) -> Result<Self, String> {
        let port = claim_port()?;
        let store = directory.join("jetstream");
        let mut configuration = String::new();
        let _ = writeln!(configuration, "host: \"127.0.0.1\"");
        let _ = writeln!(configuration, "port: {port}");
        let _ = writeln!(
            configuration,
            "jetstream {{ store_dir: \"{}\" }}",
            store.display()
        );
        configuration.push_str(extra);
        let configuration_path = directory.join("nats.conf");
        std::fs::write(&configuration_path, configuration)
            .map_err(|error| format!("the broker configuration was not written: {error}"))?;

        let log = directory.join("nats.log");
        let output = File::create(&log)
            .map_err(|error| format!("the broker log was not created: {error}"))?;
        let errors = output
            .try_clone()
            .map_err(|error| format!("the broker log was not shared: {error}"))?;
        let child = Command::new("nats-server")
            .arg("-c")
            .arg(&configuration_path)
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(errors))
            .spawn()
            .map_err(|error| format!("nats-server did not start (is it installed?): {error}"))?;
        let mut broker = Self { child, port, log };
        broker.wait_until_listening()?;
        Ok(broker)
    }

    fn wait_until_listening(&mut self) -> Result<(), String> {
        let address = SocketAddr::from(([127, 0, 0, 1], self.port));
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "nats-server exited early ({status}):\n{}",
                    self.log_text()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "nats-server never listened on {}:\n{}",
            self.port,
            self.log_text()
        ))
    }

    /// The `nats://` URL clients connect to.
    #[must_use]
    pub fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }

    /// Everything the server has logged so far, including every `Publish Violation`.
    #[must_use]
    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Closes the broker the way a crash does: the process is killed and reaped.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop();
    }
}
