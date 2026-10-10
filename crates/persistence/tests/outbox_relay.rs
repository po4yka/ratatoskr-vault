//! The relay's queries: only acknowledgement rows are due, `published_at` moves only when the
//! caller says the broker confirmed, and a failing head row backs off without starving the rows
//! behind it (XR-021 CONTRACTS.md section S02 rules 2 and 3).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use std::time::Duration;

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{
    ACKNOWLEDGEMENT_EVENT_TYPE, PolicyCommandOutcome, PolicyDecisionRecord,
};
use serde_json::json;
use uuid::Uuid;

/// Records an accepted decision for `version` and returns the acknowledgement row id.
async fn acknowledge(fixture: &TestDatabase, version: u64) -> Uuid {
    let acknowledgement_id = Uuid::now_v7();
    let envelope = json!({ "event_id": acknowledgement_id.to_string(), "marker": version });
    fixture
        .database
        .record_policy_decision(&PolicyDecisionRecord {
            command_id: Uuid::now_v7(),
            policy_version: version,
            outcome: PolicyCommandOutcome::Accepted,
            rejection_code: None,
            acknowledgement_id,
            acknowledgement: &envelope,
        })
        .await
        .expect("a recorded decision");
    acknowledgement_id
}

async fn due_ids(fixture: &TestDatabase) -> Vec<Uuid> {
    fixture
        .database
        .due_acknowledgements(10)
        .await
        .expect("the due rows")
        .into_iter()
        .map(|row| row.event_id)
        .collect()
}

/// Only acknowledgement rows are claimed, in creation order; a row whose publish failed backs off
/// and does not block the rows behind it; publishing marks a row once; the other event types are
/// never touched.
#[tokio::test]
async fn only_acknowledgements_are_claimed_and_failures_back_off() {
    let fixture = TestDatabase::create().await.expect("a database");
    let database = &fixture.database;

    // Enrolling a target writes a `vault.target.state_changed.v1` row: not the relay's business.
    database
        .ingest_delivery(
            "github",
            "repository:018f0000-0000-7000-8000-000000000501",
            "github-policy",
            Uuid::now_v7(),
            &ValidatedDelivery {
                preservation_level: "git_mirror".to_owned(),
                pinned: None,
                include_wiki: None,
                include_releases: None,
                include_issues: None,
                offsite_required: None,
                correlation_id: Uuid::now_v7().to_string(),
                policy_revision: Some(1),
            },
        )
        .await
        .expect("an enrolment");
    let first = acknowledge(&fixture, 1).await;
    let second = acknowledge(&fixture, 2).await;
    let third = acknowledge(&fixture, 3).await;

    let due = database.due_acknowledgements(10).await.expect("due rows");
    assert_eq!(
        due.iter().map(|row| row.event_id).collect::<Vec<_>>(),
        vec![first, second, third],
        "acknowledgements only, oldest first"
    );
    assert!(
        due.iter()
            .all(|row| row.event_type == ACKNOWLEDGEMENT_EVENT_TYPE)
    );
    assert_eq!(due[0].payload["marker"], 1);
    assert_eq!(
        database
            .due_acknowledgements(2)
            .await
            .expect("limited")
            .len(),
        2,
        "the limit bounds a batch"
    );

    // The head fails: it leaves the due list, the rows behind it stay.
    database
        .record_publish_failure(first, "publish_unacknowledged", Duration::from_hours(1))
        .await
        .expect("a recorded failure");
    assert_eq!(due_ids(&fixture).await, vec![second, third]);
    let (attempts, last_error, published): (i32, Option<String>, bool) = sqlx::query_as(
        "select attempt_count, last_error, published_at is not null
         from git_vault.outbox where event_id = $1",
    )
    .bind(first)
    .fetch_one(fixture.pool())
    .await
    .expect("the bookkeeping");
    assert_eq!(
        (attempts, last_error.as_deref(), published),
        (1, Some("publish_unacknowledged"), false)
    );

    // A confirmed publish marks the row; it is never due again.
    database
        .mark_outbox_published(second)
        .await
        .expect("marked published");
    assert_eq!(due_ids(&fixture).await, vec![third]);

    // A failure with no delay is due again at once, with the attempt counted.
    database
        .record_publish_failure(third, "publish_unacknowledged", Duration::ZERO)
        .await
        .expect("a recorded failure");
    assert_eq!(due_ids(&fixture).await, vec![third]);

    // Every other event type stays exactly as it was written.
    let (rows, attempted, published): (i64, i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where attempt_count <> 0),
                count(*) filter (where published_at is not null)
         from git_vault.outbox where event_type <> $1",
    )
    .bind(ACKNOWLEDGEMENT_EVENT_TYPE)
    .fetch_one(fixture.pool())
    .await
    .expect("the other rows");
    assert_eq!((rows, attempted, published), (1, 0, 0));

    fixture.cleanup().await.expect("cleanup");
}

/// The outbox accepts a closed list of event types: a type the relay has no subject for cannot be
/// inserted, instead of sitting unpublished forever (S02 rule 2).
#[tokio::test]
async fn an_outbox_event_type_outside_the_closed_list_is_refused() {
    let fixture = TestDatabase::create().await.expect("a database");

    let error = sqlx::query(
        "insert into git_vault.outbox
             (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
         values ($1, 'vault.unknown.thing.v1', 'target', '018f0000-0000-7000-8000-000000000001',
                 '{}'::jsonb, now())",
    )
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect_err("a type outside the closed list must be refused");
    assert!(
        error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_check_violation),
        "the refusal must be a CHECK violation, got {error:?}"
    );

    fixture.cleanup().await.expect("cleanup");
}
