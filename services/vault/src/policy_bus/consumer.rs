//! The command consumer: verify the Edge-provisioned durable, then decide each command and
//! settle its message (S02 rules 6 and 7).

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::consumer::{AckPolicy, PullConsumer};
use async_nats::jetstream::{self, AckKind};
use futures_util::StreamExt as _;
use ratatoskr_vault_persistence::Database;
use tokio::sync::{Notify, watch};

use super::handler::{Disposition, handle_command};
use super::{COMMAND_SUBJECT, COMMANDS_STREAM, DURABLE, LaneError};

/// The ack wait Edge provisions (S04): long enough for one reconciliation cycle.
const ACK_WAIT: Duration = Duration::from_secs(30);

/// How long a transient failure waits before the broker redelivers.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// How many commands one fetch batch may hold. Commands are decided one at a time.
const BATCH: usize = 8;

/// Fetches the durable and checks it against the contract; never creates it.
pub(super) async fn verified(client: &async_nats::Client) -> Result<PullConsumer, LaneError> {
    let context = jetstream::new(client.clone());
    let consumer: PullConsumer = context
        .get_consumer_from_stream(DURABLE, COMMANDS_STREAM)
        .await
        .map_err(|_| LaneError::Durable)?;
    let config = &consumer.cached_info().config;
    let matches_contract = config.filter_subject == COMMAND_SUBJECT
        && config.ack_policy == AckPolicy::Explicit
        && config.ack_wait == ACK_WAIT
        && config.max_deliver == -1;
    if matches_contract {
        Ok(consumer)
    } else {
        Err(LaneError::Durable)
    }
}

/// Decides commands until the connection is lost or the stream ends.
pub(super) async fn run(
    consumer: PullConsumer,
    database: Database,
    wake: Arc<Notify>,
    mut lost: watch::Receiver<bool>,
) -> Result<(), LaneError> {
    let mut messages = consumer
        .stream()
        .max_messages_per_batch(BATCH)
        .messages()
        .await
        .map_err(|_| LaneError::BusLost)?;
    loop {
        // cancel-safe: `watch::Receiver::changed` and `Stream::next` retain no partial work; the
        // arm body below runs to completion once a message was taken.
        tokio::select! {
            biased;
            _ = lost.changed() => return Err(LaneError::BusLost),
            next = messages.next() => {
                let message = next
                    .ok_or(LaneError::BusLost)?
                    .map_err(|_| LaneError::BusLost)?;
                let disposition = handle_command(&database, message.payload.as_ref()).await;
                // The decision (and its acknowledgement row) is durable before the broker is told.
                wake.notify_one();
                let kind = match disposition {
                    Disposition::Ack => AckKind::Ack,
                    Disposition::Term => AckKind::Term,
                    Disposition::Nak => AckKind::Nak(Some(RETRY_DELAY)),
                };
                message.ack_with(kind).await.map_err(|_| LaneError::BusLost)?;
            }
        }
    }
}
