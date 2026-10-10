//! The broker connection: an nkey identity read from a file at runtime, and a loss signal.

use std::sync::Arc;

use async_nats::{ConnectOptions, Event};
use ratatoskr_vault_core::config::BusConfig;
use ratatoskr_vault_http::RuntimeState;
use tokio::sync::watch;

use super::LaneError;

/// A connected client and the signal that says the connection was lost.
#[derive(Debug)]
pub(super) struct Connection {
    /// The client both lane tasks share.
    pub(super) client: async_nats::Client,
    /// Becomes `true` when the broker connection drops or is closed.
    pub(super) lost: watch::Receiver<bool>,
}

/// Connects with the configured identity.
///
/// The client reconnects on its own, and that is deliberately not relied on: the first disconnect
/// flips the bus readiness check and ends the lane, so a supervisor restarts the process instead
/// of the process advertising a lane that may be mid-reconnect (S02 rule 5).
pub(super) async fn connect(
    bus: &BusConfig,
    health: &Arc<RuntimeState>,
) -> Result<Connection, LaneError> {
    let options = match bus.nkey_seed_path.as_ref() {
        Some(path) => {
            let seed = tokio::fs::read_to_string(path)
                .await
                .map_err(|_| LaneError::Seed)?;
            ConnectOptions::with_nkey(seed.trim().to_owned())
        }
        None => ConnectOptions::new(),
    };

    let (sender, lost) = watch::channel(false);
    let health = Arc::clone(health);
    let client = options
        .event_callback(move |event| {
            let health = Arc::clone(&health);
            let sender = sender.clone();
            async move {
                if matches!(event, Event::Disconnected | Event::Closed) {
                    health.set_bus_connected(false);
                    let _ = sender.send(true);
                }
            }
        })
        .connect(bus.url.as_str())
        .await
        .map_err(|_| LaneError::Connect)?;
    Ok(Connection { client, lost })
}
