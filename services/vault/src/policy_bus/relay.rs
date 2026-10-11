//! The acknowledgement relay: publishes the acknowledgement rows the decisions left in the
//! outbox, to the one subject their type maps to, and nothing else (S02 rules 1 to 4).

use std::sync::Arc;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_vault_http::RuntimeState;
use ratatoskr_vault_persistence::{
    ACKNOWLEDGEMENT_EVENT_TYPE, Database, DueOutboxRow, PublishBacklog,
};
use ratatoskr_vault_telemetry::metrics::{OutboxPublicationOutcome, record_outbox_publication};
use tokio::sync::{Notify, watch};

use super::{ACKNOWLEDGEMENT_SUBJECT, LaneError};

/// How many rows one pass publishes.
const BATCH: u32 = 16;

/// How often the outbox is polled when nothing wakes the relay: a backed-off row becomes due on
/// its own clock.
const POLL: Duration = Duration::from_secs(1);

/// The longest a failing row waits before its next attempt.
const BACKOFF_CEILING_SECONDS: u64 = 300;

/// Failed publishes after which the oldest unpublished acknowledgement counts as stuck (XR-021
/// CONTRACTS.md section R2-03).
const STUCK_ATTEMPTS: u32 = 3;

/// How long the oldest unpublished acknowledgement may wait before it counts as stuck.
const STUCK_AGE: Duration = Duration::from_mins(5);

/// The failure class stored for a publish the broker did not confirm.
const UNACKNOWLEDGED: &str = "publish_unacknowledged";

/// What became of one attempted row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The broker confirmed the publish and the row is marked.
    Published,
    /// The broker did not confirm the publish; the row backs off.
    Refused,
}

/// Publishes due acknowledgements until the connection is lost or an outbox row is unusable, and
/// tells `health` after each pass that attempted a publish whether the relay can publish.
pub(super) async fn run(
    database: Database,
    context: jetstream::Context,
    wake: Arc<Notify>,
    mut lost: watch::Receiver<bool>,
    health: Arc<RuntimeState>,
) -> Result<(), LaneError> {
    loop {
        if *lost.borrow() {
            return Err(LaneError::BusLost);
        }
        let due = database
            .due_acknowledgements(BATCH)
            .await
            .map_err(|_| LaneError::Storage)?;
        let attempted = !due.is_empty();
        let mut refused = false;
        for row in due {
            refused |= publish(&database, &context, row).await? == Attempt::Refused;
        }
        if attempted {
            report_health(&database, &health, refused).await?;
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

/// Reports whether the relay can publish after a pass that attempted at least one row.
///
/// A pass that refused a row is unhealthy once the oldest unpublished acknowledgement has failed
/// three times or has waited more than five minutes. A pass that refused nothing is healthy once
/// no unpublished acknowledgement has a recorded failure; while a backed-off row still has one,
/// the state stays as it was, so a stuck row does not flip readiness back for the span of its
/// backoff. A failing state never stops the relay: it only flips readiness.
async fn report_health(
    database: &Database,
    health: &RuntimeState,
    refused: bool,
) -> Result<(), LaneError> {
    let backlog = database
        .publish_backlog()
        .await
        .map_err(|_| LaneError::Storage)?;
    let Some(healthy) = healthy_after(backlog, refused) else {
        return Ok(());
    };
    if health.bus_publish_healthy() != Some(healthy) {
        if healthy {
            tracing::info!("the relay publishes again");
        } else {
            tracing::error!(
                class = UNACKNOWLEDGED,
                "an acknowledgement is stuck behind refused publishes; check the NATS server log \
                 for a Publish Violation"
            );
        }
    }
    health.set_bus_publish_healthy(healthy);
    Ok(())
}

/// The relay's health after a pass, or `None` when the pass leaves it as it was.
fn healthy_after(backlog: Option<PublishBacklog>, refused: bool) -> Option<bool> {
    match backlog {
        None => Some(true),
        Some(backlog) if refused => {
            Some(backlog.oldest_attempt_count < STUCK_ATTEMPTS && backlog.oldest_age <= STUCK_AGE)
        }
        Some(backlog) => (!backlog.any_failed).then_some(true),
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
) -> Result<Attempt, LaneError> {
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
        database
            .mark_outbox_published(row.event_id)
            .await
            .map_err(|_| LaneError::Storage)?;
        record_outbox_publication(OutboxPublicationOutcome::Published);
        return Ok(Attempt::Published);
    }
    record_outbox_publication(OutboxPublicationOutcome::Failed);
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
        .map_err(|_| LaneError::Storage)?;
    Ok(Attempt::Refused)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn backlog(attempts: u32, age: Duration, any_failed: bool) -> PublishBacklog {
        PublishBacklog {
            oldest_attempt_count: attempts,
            oldest_age: age,
            any_failed,
        }
    }

    /// A refused pass is unhealthy at the third failed attempt or beyond five minutes, and not
    /// before; a clean pass recovers only when no unpublished row has failed, and leaves the
    /// state alone while a backed-off row still has.
    #[test]
    fn the_relay_is_unhealthy_only_when_the_oldest_row_is_stuck() {
        let fresh = Duration::from_secs(10);
        let after = |attempts, age, any_failed, refused| {
            healthy_after(Some(backlog(attempts, age, any_failed)), refused)
        };
        assert_eq!(after(2, fresh, true, true), Some(true));
        assert_eq!(after(3, fresh, true, true), Some(false));
        assert_eq!(
            after(1, Duration::from_mins(5), true, true),
            Some(true),
            "exactly five minutes is not older than five minutes"
        );
        assert_eq!(after(1, Duration::from_secs(301), true, true), Some(false));
        assert_eq!(healthy_after(None, false), Some(true));
        assert_eq!(after(0, fresh, false, false), Some(true));
        assert_eq!(after(3, fresh, true, false), None);
    }
}
