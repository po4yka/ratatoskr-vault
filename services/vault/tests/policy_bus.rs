//! The policy lane against a real `JetStream` broker and a disposable database: a valid command
//! enrols targets and is acknowledged exactly once, a redelivery adds nothing, a stale version is
//! rejected with the contract code, a dropped repository is withdrawn, a malformed command is
//! terminated, and the relay publishes the acknowledgement and nothing else (XR-021
//! CONTRACTS.md sections S01, S02 and S09).
//!
//! Every test resets the shared broker's two streams and the Vault durable, so they run one at a
//! time behind a process-wide lock.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_nats::jetstream;
use futures_util::StreamExt as _;
use ratatoskr_backup_contracts::{
    PolicyAcknowledged, PolicyOutcome, PolicyRejectionCode, VaultBackupPolicyApplyRequested,
};
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_vault::policy_bus::{self, COMMANDS_STREAM, DURABLE};
use ratatoskr_vault::test_support::{
    StoredAcknowledgement, acknowledgement_count, command_bytes, nats_url, provision_topology,
    publish_command, wait_for_acknowledgements, wait_until_settled,
};
use ratatoskr_vault_core::config::BusConfig;
use ratatoskr_vault_core::delivery::validate_delivery;
use ratatoskr_vault_core::policy_feed::{POLICY_SOURCE, deliveries_for_policy};
use ratatoskr_vault_http::{CheckName, CheckState, RuntimeState};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use uuid::Uuid;

const REPO_A: &str = "repository:018f0000-0000-7000-8000-000000000501";
const REPO_B: &str = "repository:018f0000-0000-7000-8000-000000000502";
const REPO_C: &str = "repository:018f0000-0000-7000-8000-000000000503";

/// How long a thing that must happen may take.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a thing that must NOT happen is watched for after the lane went quiet.
const QUIET: Duration = Duration::from_secs(2);

static SERIAL: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

/// A running lane over a fresh topology and a fresh database.
struct Lane {
    fixture: TestDatabase,
    context: jetstream::Context,
    client: async_nats::Client,
    health: Arc<RuntimeState>,
    tasks: Vec<JoinHandle<()>>,
    _serial: MutexGuard<'static, ()>,
}

impl Lane {
    async fn start() -> Self {
        let serial = SERIAL.get_or_init(|| Mutex::new(())).lock().await;
        let client = async_nats::connect(nats_url())
            .await
            .expect("the development broker must be reachable (VAULT_TEST_NATS_URL)");
        let context = provision_topology(&client)
            .await
            .expect("the topology Edge provisions");
        let fixture = TestDatabase::create().await.expect("a disposable database");
        let health = Arc::new(RuntimeState::new());
        let bus = BusConfig {
            url: nats_url(),
            nkey_seed_path: None,
        };
        let tasks = policy_bus::start(fixture.database.clone(), &bus, Arc::clone(&health))
            .await
            .expect("the lane must start against a provisioned durable");
        Self {
            fixture,
            context,
            client,
            health,
            tasks,
            _serial: serial,
        }
    }

    async fn publish(&self, command_id: Uuid, version: u64, repositories: &[&str]) -> u64 {
        let payload = command_bytes(command_id, version, repositories).expect("a command");
        publish_command(&self.context, payload, &format!("test-{}", Uuid::now_v7()))
            .await
            .expect("the command is stored")
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.fixture.pool())
            .await
            .expect("a count")
    }

    async fn finish(self) {
        for task in &self.tasks {
            task.abort();
        }
        self.fixture.cleanup().await.expect("cleanup");
    }
}

fn payload_of(acknowledgement: &StoredAcknowledgement) -> PolicyAcknowledged {
    acknowledgement
        .envelope
        .payload_as()
        .expect("a valid acknowledgement payload")
}

async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check().await {
        assert!(Instant::now() < deadline, "did not happen in time: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A valid command enrols its targets, writes inbox rows under source `github-policy`, moves the
/// last applied version and is acknowledged by exactly one event; the relay marks it published
/// only after the broker confirmed it, the event is a complete envelope answering the command,
/// and the other outbox facts stay unpublished.
#[tokio::test]
async fn a_valid_command_enrols_targets_and_is_acknowledged_exactly_once() {
    let lane = Lane::start().await;
    let command_id = Uuid::now_v7();
    let sequence = lane.publish(command_id, 1, &[REPO_A, REPO_B]).await;

    let acknowledgements = wait_for_acknowledgements(&lane.context, 1, PATIENCE)
        .await
        .expect("the acknowledgement event");
    wait_until_settled(&lane.context, sequence, PATIENCE)
        .await
        .expect("the command is settled");

    let acknowledgement = &acknowledgements[0];
    assert_eq!(
        acknowledgement.subject,
        "evt.vault.backup_policy.acknowledged.v1"
    );
    let envelope = &acknowledgement.envelope;
    assert_eq!(envelope.producer.as_str(), "ratatoskr-vault");
    assert_eq!(envelope.aggregate_id.to_wire(), "backup_policy:1");
    assert_eq!(envelope.correlation_id.to_wire(), "backup_policy:1");
    assert_eq!(
        envelope.causation_id.as_ref().map(ToString::to_string),
        Some(format!("command:{command_id}"))
    );
    assert!(envelope.tenant_id.is_none());
    assert_eq!(
        acknowledgement.message_id.as_deref(),
        Some(envelope.event_id.0.to_string().as_str()),
        "Nats-Msg-Id is the envelope's event id"
    );
    let payload = payload_of(acknowledgement);
    assert_eq!(payload.outcome, PolicyOutcome::Accepted);
    assert_eq!(payload.acknowledged_policy_version, 1);
    assert_eq!(payload.last_applied_policy_version, 0);

    assert_eq!(
        lane.fixture
            .database
            .last_applied_policy_version()
            .await
            .expect("the last applied version"),
        1
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.targets").await,
        2
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.inbox where source = 'github-policy'")
            .await,
        2
    );

    // The relay marked the acknowledgement published, and only after the PubAck.
    eventually("the acknowledgement row is marked published", async || {
        lane.scalar(
            "select count(*) from git_vault.outbox
             where event_type = 'vault.backup_policy.acknowledged.v1'
               and published_at is not null",
        )
        .await
            == 1
    })
    .await;
    // The enrolment facts exist and are untouched: no subject or ACL entry exists for them yet.
    assert_eq!(
        lane.scalar(
            "select count(*) from git_vault.outbox
             where event_type = 'vault.target.state_changed.v1'
               and published_at is null and attempt_count = 0",
        )
        .await,
        2
    );
    assert_eq!(
        lane.scalar(
            "select count(*) from git_vault.outbox
             where event_type <> 'vault.backup_policy.acknowledged.v1'
               and (published_at is not null or attempt_count <> 0)",
        )
        .await,
        0
    );
    lane.finish().await;
}

/// The broker redelivering the same command adds no target, no inbox row, no ledger row and no
/// second acknowledgement.
#[tokio::test]
async fn a_redelivered_command_adds_nothing() {
    let lane = Lane::start().await;
    let command_id = Uuid::now_v7();
    lane.publish(command_id, 1, &[REPO_A, REPO_B]).await;
    wait_for_acknowledgements(&lane.context, 1, PATIENCE)
        .await
        .expect("the first acknowledgement");

    // Same command id, same bytes, a new broker message: what an at-least-once broker does.
    let again = lane.publish(command_id, 1, &[REPO_A, REPO_B]).await;
    wait_until_settled(&lane.context, again, PATIENCE)
        .await
        .expect("the redelivery is settled");
    tokio::time::sleep(QUIET).await;

    assert_eq!(
        acknowledgement_count(&lane.context).await.expect("count"),
        1,
        "exactly one acknowledgement per command"
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.targets").await,
        2
    );
    assert_eq!(lane.scalar("select count(*) from git_vault.inbox").await, 2);
    assert_eq!(
        lane.scalar("select count(*) from git_vault.backup_policy_commands")
            .await,
        1
    );
    lane.finish().await;
}

/// A version that does not exceed the last applied one is rejected with the contract code, the
/// last applied version does not move, and the targets are untouched.
#[tokio::test]
async fn a_stale_version_is_rejected_with_the_contract_code() {
    let lane = Lane::start().await;
    lane.publish(Uuid::now_v7(), 5, &[REPO_A]).await;
    wait_for_acknowledgements(&lane.context, 1, PATIENCE)
        .await
        .expect("version 5 is acknowledged");

    let stale_command = Uuid::now_v7();
    lane.publish(stale_command, 3, &[REPO_A, REPO_B, REPO_C])
        .await;
    let acknowledgements = wait_for_acknowledgements(&lane.context, 2, PATIENCE)
        .await
        .expect("the rejection is acknowledged");

    let rejection = &acknowledgements[1];
    assert_eq!(rejection.envelope.aggregate_id.to_wire(), "backup_policy:3");
    assert_eq!(
        rejection
            .envelope
            .causation_id
            .as_ref()
            .map(ToString::to_string),
        Some(format!("command:{stale_command}"))
    );
    let payload = payload_of(rejection);
    assert_eq!(payload.outcome, PolicyOutcome::Rejected);
    assert_eq!(payload.acknowledged_policy_version, 3);
    assert_eq!(payload.last_applied_policy_version, 5);
    assert_eq!(payload.reasons.len(), 1);
    assert_eq!(
        payload.reasons[0].code,
        PolicyRejectionCode::PolicyVersionNotMonotonic
    );

    assert_eq!(
        lane.fixture
            .database
            .last_applied_policy_version()
            .await
            .expect("the last applied version"),
        5
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.targets").await,
        1,
        "a rejected document enrols nothing"
    );
    lane.finish().await;
}

/// The policy document is the whole catalog: a repository the next version omits is withdrawn,
/// one the document keeps stays governed.
#[tokio::test]
async fn a_repository_dropped_from_the_next_version_is_withdrawn() {
    let lane = Lane::start().await;
    lane.publish(Uuid::now_v7(), 1, &[REPO_A, REPO_B, REPO_C])
        .await;
    wait_for_acknowledgements(&lane.context, 1, PATIENCE)
        .await
        .expect("version 1 is acknowledged");
    lane.publish(Uuid::now_v7(), 2, &[REPO_A]).await;
    let acknowledgements = wait_for_acknowledgements(&lane.context, 2, PATIENCE)
        .await
        .expect("version 2 is acknowledged");
    assert_eq!(
        payload_of(&acknowledgements[1]).outcome,
        PolicyOutcome::Accepted
    );

    let statuses: Vec<(String, String)> = sqlx::query_as(
        "select external_repository_id, status from git_vault.targets
         order by external_repository_id",
    )
    .fetch_all(lane.fixture.pool())
    .await
    .expect("the targets");
    assert_eq!(
        statuses,
        vec![
            (REPO_A.to_owned(), "requested".to_owned()),
            (REPO_B.to_owned(), "excluded".to_owned()),
            (REPO_C.to_owned(), "excluded".to_owned()),
        ]
    );
    assert_eq!(
        lane.fixture
            .database
            .targets_governed_before("github-policy", 3)
            .await
            .expect("the governed targets"),
        vec![REPO_A.to_owned()]
    );
    lane.finish().await;
}

/// What a process that died after ingesting a version and before converging or deciding leaves
/// behind: every delivery of the command committed, no target moved, no ledger row. It runs the
/// same adapter the lane runs, so the rows are exactly the ones the lane would have written.
async fn ingest_without_converging(
    lane: &Lane,
    command_id: Uuid,
    version: u64,
    repositories: &[&str],
) {
    let bytes = command_bytes(command_id, version, repositories).expect("a command");
    let command = CommandEnvelope::from_json(&bytes).expect("a canonical command");
    let request = command
        .payload_as::<VaultBackupPolicyApplyRequested>()
        .expect("the policy payload");
    let database = &lane.fixture.database;
    let last_applied = database
        .last_applied_policy_version()
        .await
        .expect("version");
    let governed = database
        .targets_governed_before(POLICY_SOURCE, version)
        .await
        .expect("the governed targets");
    let deliveries = deliveries_for_policy(command_id, &request.policy, last_applied, &governed)
        .expect("the deliveries");
    for incoming in deliveries {
        let validated = validate_delivery(&incoming.delivery).expect("a valid delivery");
        database
            .ingest_delivery(
                &incoming.provider,
                &incoming.external_repository_id,
                &incoming.source,
                incoming.message_id,
                &validated,
            )
            .await
            .expect("the delivery committed before the crash");
    }
}

async fn target_statuses(lane: &Lane) -> Vec<(String, String)> {
    sqlx::query_as(
        "select external_repository_id, status from git_vault.targets
         order by external_repository_id",
    )
    .fetch_all(lane.fixture.pool())
    .await
    .expect("the targets")
}

/// A crash after the ingests of a version leaves its deliveries committed and its targets
/// unmoved. The redelivery must finish the job: the dropped repository ends excluded although its
/// withdrawal was already written, the kept one stays governed, and exactly one acknowledgement
/// answers the command.
#[tokio::test]
async fn a_redelivery_after_a_crash_still_excludes_the_dropped_repository() {
    let lane = Lane::start().await;
    lane.publish(Uuid::now_v7(), 1, &[REPO_A, REPO_B]).await;
    wait_for_acknowledgements(&lane.context, 1, PATIENCE)
        .await
        .expect("version 1 is acknowledged");

    let command_id = Uuid::now_v7();
    ingest_without_converging(&lane, command_id, 2, &[REPO_A]).await;
    assert_eq!(
        target_statuses(&lane).await,
        vec![
            (REPO_A.to_owned(), "requested".to_owned()),
            (REPO_B.to_owned(), "requested".to_owned()),
        ],
        "the crash left both targets unmoved"
    );

    let sequence = lane.publish(command_id, 2, &[REPO_A]).await;
    let acknowledgements = wait_for_acknowledgements(&lane.context, 2, PATIENCE)
        .await
        .expect("version 2 is acknowledged");
    wait_until_settled(&lane.context, sequence, PATIENCE)
        .await
        .expect("the redelivery is settled");
    tokio::time::sleep(QUIET).await;

    assert_eq!(
        payload_of(&acknowledgements[1]).outcome,
        PolicyOutcome::Accepted
    );
    assert_eq!(
        target_statuses(&lane).await,
        vec![
            (REPO_A.to_owned(), "requested".to_owned()),
            (REPO_B.to_owned(), "excluded".to_owned()),
        ],
        "the redelivery converged the withdrawn repository"
    );
    assert_eq!(
        acknowledgement_count(&lane.context).await.expect("count"),
        2,
        "exactly one acknowledgement per command"
    );
    lane.finish().await;
}

/// The same crash window for a repository the next version brings back: it was excluded by an
/// earlier version, its new revision is committed, and only the redelivery can reactivate it.
#[tokio::test]
async fn a_redelivery_after_a_crash_still_reactivates_a_readded_repository() {
    let lane = Lane::start().await;
    lane.publish(Uuid::now_v7(), 1, &[REPO_A, REPO_B]).await;
    lane.publish(Uuid::now_v7(), 2, &[REPO_A]).await;
    wait_for_acknowledgements(&lane.context, 2, PATIENCE)
        .await
        .expect("versions 1 and 2 are acknowledged");

    let command_id = Uuid::now_v7();
    ingest_without_converging(&lane, command_id, 3, &[REPO_A, REPO_B]).await;
    assert_eq!(
        target_statuses(&lane).await,
        vec![
            (REPO_A.to_owned(), "requested".to_owned()),
            (REPO_B.to_owned(), "excluded".to_owned()),
        ],
        "the crash left the excluded repository excluded"
    );

    let sequence = lane.publish(command_id, 3, &[REPO_A, REPO_B]).await;
    wait_for_acknowledgements(&lane.context, 3, PATIENCE)
        .await
        .expect("version 3 is acknowledged");
    wait_until_settled(&lane.context, sequence, PATIENCE)
        .await
        .expect("the redelivery is settled");
    tokio::time::sleep(QUIET).await;

    assert_eq!(
        target_statuses(&lane).await,
        vec![
            (REPO_A.to_owned(), "requested".to_owned()),
            (REPO_B.to_owned(), "requested".to_owned()),
        ],
        "the redelivery reactivated the re-added repository"
    );
    assert_eq!(
        acknowledgement_count(&lane.context).await.expect("count"),
        3,
        "exactly one acknowledgement per command"
    );
    lane.finish().await;
}

/// Input that can never become valid is terminated, not retried and not acknowledged: bytes that
/// are no envelope, an envelope from another producer, and an envelope carrying a tenant.
#[tokio::test]
async fn malformed_commands_are_terminated_and_leave_no_trace() {
    let lane = Lane::start().await;
    let mut advisories = lane
        .client
        .subscribe(format!(
            "$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.{COMMANDS_STREAM}.{DURABLE}"
        ))
        .await
        .expect("the termination advisory subject");
    lane.client.flush().await.expect("the subscription is live");

    let valid = String::from_utf8(command_bytes(Uuid::now_v7(), 9, &[REPO_A]).expect("a command"))
        .expect("utf-8");
    let wrong_producer = valid.replace(
        "\"producer\": \"ratatoskr-github\"",
        "\"producer\": \"ratatoskr-x\"",
    );
    assert_ne!(wrong_producer, valid, "the producer was replaced");
    let with_tenant = valid.replace(
        "\"schema_version\": 1",
        "\"tenant_id\": \"user:018f0000-0000-7000-8000-000000000001\", \"schema_version\": 1",
    );
    assert_ne!(with_tenant, valid, "a tenant was added");

    let mut last = 0;
    for bytes in [
        b"not json at all".to_vec(),
        wrong_producer.into_bytes(),
        with_tenant.into_bytes(),
    ] {
        last = publish_command(&lane.context, bytes, &format!("bad-{}", Uuid::now_v7()))
            .await
            .expect("stored");
    }
    wait_until_settled(&lane.context, last, PATIENCE)
        .await
        .expect("all three are settled");

    for _ in 0..3 {
        tokio::time::timeout(PATIENCE, advisories.next())
            .await
            .expect("a termination advisory per message")
            .expect("the advisory subscription is open");
    }
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        acknowledgement_count(&lane.context).await.expect("count"),
        0
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.targets").await,
        0
    );
    assert_eq!(
        lane.scalar("select count(*) from git_vault.backup_policy_commands")
            .await,
        0
    );
    lane.finish().await;
}

/// A lane that started against a verified durable reports the bus ready, and the process-level
/// readiness is the conjunction of its checks.
#[tokio::test]
async fn a_started_lane_reports_a_connected_bus() {
    let lane = Lane::start().await;
    let checks = lane.health.checks();
    let bus = checks
        .iter()
        .find(|check| check.name == CheckName::Bus)
        .expect("a configured bus is a named readiness check");
    assert_eq!(bus.state, CheckState::Pass);
    lane.finish().await;
}
