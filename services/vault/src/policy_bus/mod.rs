//! The policy lane: GitHub's desired backup policy arrives as a command on the bus, Vault applies
//! it through the reconciliation cycle, and exactly one acknowledgement event answers it
//! (XR-021 CONTRACTS.md sections S01 to S04 and S09).
//!
//! Two supervised tasks run beside the listeners. The consumer fetches the Edge-provisioned
//! durable and decides each command; the relay publishes the acknowledgements those decisions
//! left in the outbox, and nothing else. Either task returning before an orderly shutdown ends the
//! process (S02 rule 5): the lane marks the bus check failed and the harness exits non-zero.

mod connection;
mod consumer;
mod handler;
mod relay;

use std::future::Future;
use std::sync::Arc;

use ratatoskr_vault_core::config::BusConfig;
use ratatoskr_vault_http::RuntimeState;
use ratatoskr_vault_persistence::Database;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// The stream Edge provisions for commands.
pub const COMMANDS_STREAM: &str = "ratatoskr_commands";

/// The stream Edge provisions for events.
pub const EVENTS_STREAM: &str = "ratatoskr_events";

/// The durable Edge provisions for this lane (S04).
pub const DURABLE: &str = "ratatoskr_vault_backup_policy";

/// The subject GitHub's policy command is published on.
pub const COMMAND_SUBJECT: &str = "cmd.vault.backup_policy.apply_requested.v1";

/// The subject Vault's acknowledgement is published on.
pub const ACKNOWLEDGEMENT_SUBJECT: &str = "evt.vault.backup_policy.acknowledged.v1";

/// Why a lane task stopped. Every variant renders without an endpoint, a path, a credential or
/// any message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LaneError {
    /// The nkey seed file could not be read.
    #[error("the nkey seed file could not be read; check RATATOSKR__BUS__NKEY_SEED_PATH")]
    Seed,
    /// The broker could not be reached or refused the identity.
    #[error("the broker could not be reached; check RATATOSKR__BUS__URL and the nkey identity")]
    Connect,
    /// The durable is missing or differs from the contract.
    #[error(
        "the durable ratatoskr_vault_backup_policy is missing or differs from its contract; \
         start Edge first, it provisions the durable"
    )]
    Durable,
    /// The broker connection was lost.
    #[error("the broker connection was lost")]
    BusLost,
    /// The database failed.
    #[error("the database failed")]
    Storage,
    /// An outbox row the relay selected is not a publishable acknowledgement.
    #[error("an outbox row is not a publishable acknowledgement")]
    StoredEnvelope,
}

/// Connects, verifies the durable, and starts the consumer and the relay.
///
/// # Errors
///
/// A safe description when the broker cannot be reached or the durable does not match the
/// contract; the process must not start then.
pub async fn start(
    database: Database,
    bus: &BusConfig,
    health: Arc<RuntimeState>,
) -> Result<Vec<JoinHandle<()>>, String> {
    let connection = connection::connect(bus, &health)
        .await
        .map_err(|error| error.to_string())?;
    let durable = consumer::verified(&connection.client)
        .await
        .map_err(|error| error.to_string())?;
    health.set_bus_connected(true);

    let wake = Arc::new(Notify::new());
    let consuming = tokio::spawn(supervised(
        "consumer",
        Arc::clone(&health),
        consumer::run(
            durable,
            database.clone(),
            Arc::clone(&wake),
            connection.lost.clone(),
        ),
    ));
    let relaying = tokio::spawn(supervised(
        "relay",
        health,
        relay::run(
            database,
            async_nats::jetstream::new(connection.client),
            wake,
            connection.lost,
        ),
    ));
    Ok(vec![consuming, relaying])
}

/// Runs one lane task to its end and reports it. A lane task has no orderly end of its own: the
/// process stops it by aborting, so returning at all means its work has stopped, and the bus check
/// fails before the harness starts draining.
async fn supervised(
    task: &'static str,
    health: Arc<RuntimeState>,
    work: impl Future<Output = Result<(), LaneError>>,
) {
    let outcome = work.await;
    health.set_bus_connected(false);
    if let Err(error) = outcome {
        tracing::error!(task, %error, "the policy lane stopped");
    } else {
        tracing::error!(task, "the policy lane stopped without an error");
    }
}
