//! The queries behind the acknowledgement relay: which outbox rows are due, when one is
//! published, and how a failure backs off (XR-021 CONTRACTS.md section S02 rules 2 and 3).
//!
//! The relay is closed over one event type. Every other outbox event type stays unpublished and
//! unchanged until its subject and ACL entry exist.

use std::time::Duration;

use ratatoskr_vault_core::error::VaultError;
use uuid::Uuid;

use crate::{Database, classify_status_update_failure};

/// The one event type the relay publishes.
pub const ACKNOWLEDGEMENT_EVENT_TYPE: &str = "vault.backup_policy.acknowledged.v1";

/// One outbox row the relay may publish.
#[derive(Debug, Clone, PartialEq)]
pub struct DueOutboxRow {
    /// The row id, which is also the envelope's `event_id` and the `Nats-Msg-Id`.
    pub event_id: Uuid,
    /// The stored event type.
    pub event_type: String,
    /// The stored canonical envelope.
    pub payload: serde_json::Value,
    /// How many publishes of this row have failed so far.
    pub attempt_count: u32,
}

/// One due row straight from the query; positional order must match the SELECT.
type DueRow = (Uuid, String, serde_json::Value, i32);

/// What the relay's health decision reads about the unpublished acknowledgements (XR-021
/// CONTRACTS.md section R2-03).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PublishBacklog {
    /// Failed publishes recorded on the oldest unpublished acknowledgement.
    pub oldest_attempt_count: u32,
    /// How long the oldest unpublished acknowledgement has waited, by the database clock.
    pub oldest_age: Duration,
    /// Whether any unpublished acknowledgement has a recorded failed publish.
    pub any_failed: bool,
}

/// The backlog summary straight from the query; positional order must match the SELECT.
type BacklogRow = (Option<i32>, Option<f64>, bool);

impl Database {
    /// The acknowledgement rows due for publication, oldest first, at most `limit`.
    ///
    /// A row whose last publish failed is due again only after its `next_attempt_at`, so one
    /// failing head row never starves the rows behind it.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn due_acknowledgements(&self, limit: u32) -> Result<Vec<DueOutboxRow>, VaultError> {
        let rows: Vec<DueRow> = sqlx::query_as(
            "select event_id, event_type, payload, attempt_count
             from git_vault.outbox
             where event_type = $1
               and published_at is null
               and (next_attempt_at is null or next_attempt_at <= now())
             order by created_at, event_id
             limit $2",
        )
        .bind(ACKNOWLEDGEMENT_EVENT_TYPE)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;
        Ok(rows
            .into_iter()
            .map(|(event_id, event_type, payload, attempts)| DueOutboxRow {
                event_id,
                event_type,
                payload,
                attempt_count: u32::try_from(attempts).unwrap_or(0),
            })
            .collect())
    }

    /// Marks a row published. Called only after the broker's `PubAck` resolved.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn mark_outbox_published(&self, event_id: Uuid) -> Result<(), VaultError> {
        sqlx::query(
            "update git_vault.outbox set published_at = now()
             where event_id = $1 and published_at is null",
        )
        .bind(event_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|error| classify_status_update_failure(&error))
    }

    /// Records a failed publish: the attempt count rises, a safe failure class is kept and the
    /// row is not due again before `retry_after` has passed.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn record_publish_failure(
        &self,
        event_id: Uuid,
        failure_class: &str,
        retry_after: Duration,
    ) -> Result<(), VaultError> {
        sqlx::query(
            "update git_vault.outbox
             set attempt_count = attempt_count + 1,
                 last_error = $2,
                 next_attempt_at = now() + make_interval(secs => $3)
             where event_id = $1",
        )
        .bind(event_id)
        .bind(failure_class)
        .bind(retry_after.as_secs_f64())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|error| classify_status_update_failure(&error))
    }

    /// A summary of the unpublished acknowledgements, or `None` when there are none. Rows that
    /// are backed off count: the summary describes the backlog, not the due batch.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn publish_backlog(&self) -> Result<Option<PublishBacklog>, VaultError> {
        let row: Option<BacklogRow> = sqlx::query_as(
            "select
                 (select attempt_count from git_vault.outbox
                   where event_type = $1 and published_at is null
                   order by created_at, event_id limit 1),
                 (select extract(epoch from now() - created_at)::float8 from git_vault.outbox
                   where event_type = $1 and published_at is null
                   order by created_at, event_id limit 1),
                 exists (select 1 from git_vault.outbox
                          where event_type = $1 and published_at is null and attempt_count > 0)",
        )
        .bind(ACKNOWLEDGEMENT_EVENT_TYPE)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| classify_status_update_failure(&error))?;
        Ok(row.and_then(|(attempts, age, any_failed)| {
            // The scalar subqueries are null together exactly when no row is unpublished.
            attempts.zip(age).map(|(attempts, age)| PublishBacklog {
                oldest_attempt_count: u32::try_from(attempts).unwrap_or(0),
                oldest_age: Duration::from_secs_f64(age.max(0.0)),
                any_failed,
            })
        }))
    }
}
