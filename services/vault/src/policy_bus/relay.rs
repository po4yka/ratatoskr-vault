//! The acknowledgement relay: publishes the acknowledgement rows the decisions left in the
//! outbox, to the one subject their type maps to, and nothing else (S02 rules 1 to 4).

use std::sync::Arc;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_vault_persistence::{ACKNOWLEDGEMENT_EVENT_TYPE, Database, DueOutboxRow};
use tokio::sync::{Notify, watch};

use super::{ACKNOWLEDGEMENT_SUBJECT, LaneError};

/// How many rows one pass publishes.
const BATCH: u32 = 16;

/// How often the outbox is polled when nothing wakes the relay: a backed-off row becomes due on
/// its own clock.
const POLL: Duration = Duration::from_secs(1);

/// The longest a failing row waits before its next attempt.
const BACKOFF_CEILING_SECONDS: u64 = 300;

/// The failure class stored for a publish the broker did not confirm.
const UNACKNOWLEDGED: &str = "publish_unacknowledged";

/// Publishes due acknowledgements until the connection is lost or an outbox row is unusable.
pub(super) async fn run(
    database: Database,
    context: jetstream::Context,
    wake: Arc<Notify>,
    mut lost: watch::Receiver<bool>,
) -> Result<(), LaneError> {
    loop {
        if *lost.borrow() {
            return Err(LaneError::BusLost);
        }
        let due = database
            .due_acknowledgements(BATCH)
            .await
            .map_err(|_| LaneError::Storage)?;
        for row in due {
            publish(&database, &context, row).await?;
        }
        // cancel-safe: `watch::Receiver::changed`, `Notify::notified` and `sleep` retain no work.
        tokio::select! {
            biased;
            _ = lost.changed() => return Err(LaneError::BusLost),
            () = wake.notified() => {}
            () = tokio::time::sleep(POLL) => {}
        }
    }
}

/// The subject an outbox event type publishes on. A CLOSED match: a type with no subject is a
/// programming error that stops the worker, never a row that is silently skipped.
fn subject_for(event_type: &str) -> Result<&'static str, LaneError> {
    match event_type {
        ACKNOWLEDGEMENT_EVENT_TYPE => Ok(ACKNOWLEDGEMENT_SUBJECT),
        _ => Err(LaneError::StoredEnvelope),
    }
}

/// Publishes one row and marks it only after the broker's `PubAck`.
async fn publish(
    database: &Database,
    context: &jetstream::Context,
    row: DueOutboxRow,
) -> Result<(), LaneError> {
    let subject = subject_for(&row.event_type)?;
    let envelope: EventEnvelope =
        serde_json::from_value(row.payload).map_err(|_| LaneError::StoredEnvelope)?;
    if envelope.event_id.0 != row.event_id {
        return Err(LaneError::StoredEnvelope);
    }
    let body = envelope
        .to_canonical_json()
        .map_err(|_| LaneError::StoredEnvelope)?;

    let mut headers = HeaderMap::new();
    headers.insert("Nats-Msg-Id", row.event_id.to_string());
    let confirmed = match context
        .publish_with_headers(subject, headers, body.into_bytes().into())
        .await
    {
        Ok(pending) => pending.await.is_ok(),
        Err(_) => false,
    };

    if confirmed {
        return database
            .mark_outbox_published(row.event_id)
            .await
            .map_err(|_| LaneError::Storage);
    }
    // A PubAck that never arrives looks the same as a permission denial: the broker only logs it.
    tracing::warn!(
        class = UNACKNOWLEDGED,
        attempt = row.attempt_count,
        "the acknowledgement was not confirmed by the broker; check the NATS server log for a \
         Publish Violation"
    );
    database
        .record_publish_failure(row.event_id, UNACKNOWLEDGED, backoff(row.attempt_count))
        .await
        .map_err(|_| LaneError::Storage)
}

/// Capped exponential backoff: one second, two, four, up to five minutes.
fn backoff(attempts: u32) -> Duration {
    let seconds = 1_u64
        .checked_shl(attempts)
        .map_or(BACKOFF_CEILING_SECONDS, |step| {
            step.min(BACKOFF_CEILING_SECONDS)
        });
    Duration::from_secs(seconds)
}
