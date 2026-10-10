//! The Vault NATS identity against a broker that enforces it: `deploy/nats/identity.conf` is the
//! VAULT stanza of XR-021 CONTRACTS.md section S03, and a real authorization-enabled
//! `nats-server` built from that file lets the Vault identity fetch and acknowledge a policy
//! command and publish its acknowledgement, and refuses everything else.
//!
//! A denied publish is invisible to the client (the server only logs a `Publish Violation`), so
//! each refusal is observed twice: as the missing acknowledgement or response, and as the line in
//! the server log.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_nats::jetstream::{self, consumer};
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_identifiers::WireTimestamp;
use ratatoskr_vault::policy_bus::{
    ACKNOWLEDGEMENT_SUBJECT, COMMANDS_STREAM, DURABLE, EVENTS_STREAM,
};
use ratatoskr_vault::test_support::{
    Broker, command_bytes, provision_topology, publish_command, wait_for_acknowledgements,
    wait_until_settled,
};
use ratatoskr_vault_core::policy_feed::{PolicyDecision, acknowledgement_envelope};
use uuid::Uuid;

const REPO_A: &str = "repository:018f0000-0000-7000-8000-000000000501";

/// How long a refused request may take to be declared refused.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// The VAULT stanza of `platform/deploy/nats/ratatoskr.conf` (S03), as the equality check of the
/// workspace compares it: comments, whitespace and the nkey token ignored.
const EXPECTED_STANZA: &str = r#"
{
    nkey: UREPLACE_ME_WITH_THE_PUBLIC_NKEY_OF_RATATOSKR_VAULT_XXXXXXXX
    permissions: {
        publish: {
            allow: [
                "evt.vault.backup_policy.acknowledged.v1",
                "$JS.API.CONSUMER.INFO.ratatoskr_commands.ratatoskr_vault_backup_policy",
                "$JS.API.CONSUMER.MSG.NEXT.ratatoskr_commands.ratatoskr_vault_backup_policy",
                "$JS.ACK.ratatoskr_commands.ratatoskr_vault_backup_policy.>",
            ]
        }
        subscribe: { allow: ["_INBOX.>"] }
    }
}
"#;

fn fragment_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/nats/identity.conf")
}

fn fragment() -> String {
    let path = fragment_path();
    assert!(
        path.is_file(),
        "{} is missing: Vault must carry its NATS identity fragment (S03)",
        path.display()
    );
    std::fs::read_to_string(&path).expect("the fragment is readable")
}

/// Drops comment lines and trailing comments, blanks the nkey token, and removes whitespace.
fn normalise(text: &str) -> String {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .map(|line| {
            if line.starts_with("nkey:") {
                "nkey:TOKEN"
            } else {
                line
            }
        })
        .collect::<String>()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Replaces the placeholder with a real public key so the server accepts the identity.
fn with_public_key(fragment: &str, public_key: &str) -> String {
    fragment
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("nkey:") {
                format!("    nkey: {public_key}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fragment equals the VAULT stanza of S03, so the deployed ACL and this file cannot drift.
#[test]
fn the_fragment_is_the_vault_stanza_of_the_reviewed_acl() {
    assert_eq!(normalise(&fragment()), normalise(EXPECTED_STANZA));
}

async fn connect(url: &str, seed: &str) -> async_nats::Client {
    async_nats::ConnectOptions::with_nkey(seed.to_owned())
        .request_timeout(Some(REQUEST_TIMEOUT))
        .connect(url)
        .await
        .expect("the nkey identity connects")
}

/// Polls the server log until it contains `needle`.
async fn log_shows(broker: &Broker, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if broker.log_text().contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// The Vault identity fetches and acknowledges a policy command and publishes its acknowledgement
/// through a broker that enforces the fragment; any other `evt.*` publish and `CONSUMER.INFO` on a
/// foreign durable are refused.
#[tokio::test]
async fn the_vault_identity_does_its_job_and_nothing_else() {
    let administrator = nkeys::KeyPair::new_user();
    let vault = nkeys::KeyPair::new_user();
    let admin_seed = administrator.seed().expect("an admin seed");
    let vault_seed = vault.seed().expect("a vault seed");
    let stanza = with_public_key(&fragment(), &vault.public_key());
    let authorization = format!(
        "authorization {{\n  users: [\n    {{ nkey: {admin}\n      permissions: {{\n        \
         publish: {{ allow: [\">\"] }}\n        subscribe: {{ allow: [\">\"] }}\n      }} }},\n\
         {stanza}\n  ]\n}}\n",
        admin = administrator.public_key(),
    );
    let directory =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("authz-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    let broker = Broker::spawn(&directory, &authorization).expect("an authorizing broker");

    // Edge's side: provision the topology and publish the command.
    let admin_client = connect(&broker.url(), &admin_seed).await;
    let admin = provision_topology(&admin_client).await.expect("topology");
    let command = command_bytes(Uuid::now_v7(), 1, &[REPO_A]).expect("a command");
    let sequence = publish_command(&admin, command.clone(), "authorized-test")
        .await
        .expect("the command is stored");
    let answer = acknowledgement_envelope(
        &CommandEnvelope::from_json(&command).expect("the command parses"),
        Uuid::now_v7(),
        WireTimestamp::now(),
        &PolicyDecision::for_version(1, 0),
    )
    .expect("an acknowledgement")
    .to_canonical_json()
    .expect("its canonical form");

    // Vault's side, with only the fragment's permissions.
    let vault_client = connect(&broker.url(), &vault_seed).await;
    let context = jetstream::context::ContextBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .build(vault_client.clone());
    let durable: consumer::PullConsumer = context
        .get_consumer_from_stream(DURABLE, COMMANDS_STREAM)
        .await
        .expect("CONSUMER.INFO on its own durable is allowed");
    let mut messages = durable
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(5))
        .messages()
        .await
        .expect("CONSUMER.MSG.NEXT on its own durable is allowed");
    let message = futures_util::StreamExt::next(&mut messages)
        .await
        .expect("a command is delivered")
        .expect("the delivery is well formed");
    message
        .double_ack()
        .await
        .expect("$JS.ACK on its own durable is allowed");
    context
        .publish(ACKNOWLEDGEMENT_SUBJECT, answer.into())
        .await
        .expect("the acknowledgement subject is publishable")
        .await
        .expect("the broker confirmed the acknowledgement");

    wait_until_settled(&admin, sequence, Duration::from_secs(10))
        .await
        .expect("the command was acknowledged through the fragment's $JS.ACK permission");
    let stored = wait_for_acknowledgements_raw(&admin, 1).await;
    assert_eq!(
        stored, 1,
        "exactly the permitted acknowledgement was stored"
    );

    // Refusal 1: another evt.* subject. No PubAck ever arrives, and the server says why.
    let refused = context
        .publish("evt.vault.target.state_changed.v1", "{}".into())
        .await
        .expect("the publish itself is sent")
        .await;
    assert!(
        refused.is_err(),
        "a publish outside the allow list must not be acknowledged"
    );
    assert!(
        log_shows(&broker, "Publish Violation").await
            && log_shows(&broker, "evt.vault.target.state_changed.v1").await,
        "the server log must record the violation\n{}",
        broker.log_text()
    );
    assert_eq!(wait_for_acknowledgements_raw(&admin, 1).await, 1);

    // Refusal 2: CONSUMER.INFO on a durable that belongs to another identity.
    let foreign: Result<consumer::PullConsumer, _> = context
        .get_consumer_from_stream("ratatoskr_github_policy_acknowledged", EVENTS_STREAM)
        .await;
    assert!(
        foreign.is_err(),
        "a foreign durable must not be inspectable"
    );
    assert!(
        log_shows(
            &broker,
            "$JS.API.CONSUMER.INFO.ratatoskr_events.ratatoskr_github_policy_acknowledged"
        )
        .await,
        "the server log must record the foreign CONSUMER.INFO\n{}",
        broker.log_text()
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// The number of events the stream holds, through the helper that waits for at least `count`.
async fn wait_for_acknowledgements_raw(context: &jetstream::Context, count: u64) -> usize {
    wait_for_acknowledgements(context, count, Duration::from_secs(10))
        .await
        .expect("acknowledgements")
        .len()
}
