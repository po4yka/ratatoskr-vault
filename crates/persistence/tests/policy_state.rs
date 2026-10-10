//! The state the policy lane needs from the database: which targets a source governs, the
//! contract correlation text surviving verbatim, and the command ledger that answers "what was
//! the last applied policy version" and "did I already decide this command" (XR-021
//! CONTRACTS.md section S09).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::target_state::TargetStatus;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use ratatoskr_vault_persistence::{PolicyCommandOutcome, PolicyDecisionRecord, RecordedDecision};
use serde_json::json;
use uuid::Uuid;

const SOURCE: &str = "github-policy";
const REPO_A: &str = "repository:018f0000-0000-7000-8000-000000000501";
const REPO_B: &str = "repository:018f0000-0000-7000-8000-000000000502";
const REPO_C: &str = "repository:018f0000-0000-7000-8000-000000000503";

async fn test_database() -> TestDatabase {
    TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied")
}

/// The record a policy delivery carries: the contract correlation text and the policy version as
/// the revision.
fn delivery(level: &str, revision: u64) -> ValidatedDelivery {
    ValidatedDelivery {
        preservation_level: level.to_owned(),
        pinned: None,
        include_wiki: None,
        include_releases: None,
        include_issues: None,
        offsite_required: None,
        correlation_id: format!("backup_policy:{revision}"),
        policy_revision: Some(revision),
    }
}

/// A policy delivery is ingestible although its correlation is not a UUID, the revision keeps the
/// source and the text verbatim, and only targets whose governing revision came from the source
/// at a level above `none` are listed as governed by it.
#[tokio::test]
async fn ingest_keeps_source_and_text_correlation_and_lists_governed_targets() {
    let fixture = test_database().await;
    let database = &fixture.database;

    let first = database
        .ingest_delivery(
            "github",
            REPO_A,
            SOURCE,
            Uuid::now_v7(),
            &delivery("git_mirror", 7),
        )
        .await;
    assert!(
        first.is_ok(),
        "a backup_policy:<version> correlation must ingest, got {first:?}"
    );
    let target_a = first.expect("ingested");
    database
        .ingest_delivery(
            "github",
            REPO_B,
            SOURCE,
            Uuid::now_v7(),
            &delivery("git_mirror", 7),
        )
        .await
        .expect("a second policy repository");
    database
        .ingest_delivery(
            "github",
            REPO_C,
            SOURCE,
            Uuid::now_v7(),
            &delivery("none", 7),
        )
        .await
        .expect("a withdrawn policy repository");

    let (source, correlation): (Option<String>, String) = sqlx::query_as(
        "select source, correlation_id from git_vault.desired_state_revisions
         where target_id = $1 and policy_revision = 7",
    )
    .bind(target_a)
    .fetch_one(fixture.pool())
    .await
    .expect("the revision evidence");
    assert_eq!(source.as_deref(), Some(SOURCE));
    assert_eq!(correlation, "backup_policy:7");

    // A and B are mirrored by the policy; C was already withdrawn, so it is not "governed".
    let governed = database
        .targets_governed_before(SOURCE, 8)
        .await
        .expect("the governed list");
    assert_eq!(governed, vec![REPO_A.to_owned(), REPO_B.to_owned()]);

    // A newer revision from another source takes the target over.
    database
        .ingest_delivery(
            "github",
            REPO_B,
            "github-catalog",
            Uuid::now_v7(),
            &delivery("git_mirror", 8),
        )
        .await
        .expect("a newer revision from another source");
    assert_eq!(
        database
            .targets_governed_before(SOURCE, 8)
            .await
            .expect("the governed list"),
        vec![REPO_A.to_owned()]
    );

    // The text correlation also flows through the guarded transition and the retention evidence.
    database
        .apply_transition(target_a, TargetStatus::Excluded, &delivery("none", 9))
        .await
        .expect("a withdrawal with a text correlation converges");

    fixture.cleanup().await.expect("cleanup");
}

/// The governed list is a function of the state before the command: the withdrawal a command
/// wrote itself does not hide the repository from the same command's redelivery, and a later
/// command no longer lists it.
#[tokio::test]
async fn the_governed_list_ignores_the_source_s_own_revisions_from_the_asked_version() {
    let fixture = test_database().await;
    let database = &fixture.database;
    for (level, revision) in [("git_mirror", 1), ("none", 2)] {
        database
            .ingest_delivery(
                "github",
                REPO_A,
                SOURCE,
                Uuid::now_v7(),
                &delivery(level, revision),
            )
            .await
            .expect("a policy delivery");
    }

    assert_eq!(
        database
            .targets_governed_before(SOURCE, 2)
            .await
            .expect("list"),
        vec![REPO_A.to_owned()],
        "version 2 withdrew the repository, and asking before version 2 still lists it"
    );
    assert_eq!(
        database
            .targets_governed_before(SOURCE, 3)
            .await
            .expect("list"),
        Vec::<String>::new(),
        "after version 2 the repository is withdrawn"
    );
    assert_eq!(
        database
            .targets_governed_before(SOURCE, 1)
            .await
            .expect("list"),
        Vec::<String>::new(),
        "before version 1 nothing was governed"
    );

    let target = database
        .repository_target("github", REPO_A)
        .await
        .expect("a known repository resolves to its target");
    let again = database
        .repository_target("github", REPO_A)
        .await
        .expect("and resolves to the same one");
    assert_eq!(target, again);
    assert_eq!(
        database.repository_target("github", REPO_B).await,
        Err(ratatoskr_vault_core::error::VaultError::InvalidDelivery { field: "target_id" })
    );
    fixture.cleanup().await.expect("cleanup");
}

/// How many acknowledgement rows the outbox holds, with the aggregate and marker of the first.
async fn acknowledgement_rows(fixture: &TestDatabase) -> (i64, Option<String>, Option<String>) {
    sqlx::query_as(
        "select count(*), min(aggregate_id::text), min(payload->>'marker')
         from git_vault.outbox where event_type = 'vault.backup_policy.acknowledged.v1'",
    )
    .fetch_one(fixture.pool())
    .await
    .expect("the acknowledgement rows")
}

/// A decision is recorded once with its acknowledgement; a redelivery writes nothing; a
/// rejection does not move the last applied version; two accepted rows for one version cannot
/// exist.
#[tokio::test]
async fn a_policy_decision_is_recorded_once_with_its_acknowledgement() {
    let fixture = test_database().await;
    let database = &fixture.database;
    assert_eq!(
        database
            .last_applied_policy_version()
            .await
            .expect("version"),
        0
    );

    let command_id = Uuid::now_v7();
    assert!(
        !database
            .policy_command_recorded(command_id)
            .await
            .expect("lookup")
    );

    let acknowledgement_id = Uuid::now_v7();
    let envelope = json!({ "event_id": acknowledgement_id.to_string(), "marker": "accepted-7" });
    let accepted = PolicyDecisionRecord {
        command_id,
        policy_version: 7,
        outcome: PolicyCommandOutcome::Accepted,
        rejection_code: None,
        acknowledgement_id,
        acknowledgement: &envelope,
    };
    assert_eq!(
        database
            .record_policy_decision(&accepted)
            .await
            .expect("record"),
        RecordedDecision::Recorded
    );
    assert_eq!(
        database
            .last_applied_policy_version()
            .await
            .expect("version"),
        7
    );
    assert!(
        database
            .policy_command_recorded(command_id)
            .await
            .expect("lookup")
    );
    assert_eq!(
        acknowledgement_rows(&fixture).await,
        (
            1,
            Some("backup_policy:7".to_owned()),
            Some("accepted-7".to_owned())
        )
    );

    // The same command again: nothing is written, not even a second acknowledgement.
    assert_eq!(
        database
            .record_policy_decision(&accepted)
            .await
            .expect("record again"),
        RecordedDecision::AlreadyRecorded
    );
    assert_eq!(acknowledgement_rows(&fixture).await.0, 1);

    // A rejection is recorded and acknowledged but does not move the last applied version.
    let stale_envelope = json!({ "marker": "rejected-5" });
    let stale = PolicyDecisionRecord {
        command_id: Uuid::now_v7(),
        policy_version: 5,
        outcome: PolicyCommandOutcome::Rejected,
        rejection_code: Some("policy_version_not_monotonic"),
        acknowledgement_id: Uuid::now_v7(),
        acknowledgement: &stale_envelope,
    };
    assert_eq!(
        database
            .record_policy_decision(&stale)
            .await
            .expect("record"),
        RecordedDecision::Recorded
    );
    assert_eq!(
        database
            .last_applied_policy_version()
            .await
            .expect("version"),
        7
    );
    assert_eq!(acknowledgement_rows(&fixture).await.0, 2);

    // A second accepted command for an already accepted version is impossible, and the refusal
    // leaves no acknowledgement behind.
    let duplicate = PolicyDecisionRecord {
        command_id: Uuid::now_v7(),
        acknowledgement_id: Uuid::now_v7(),
        ..accepted
    };
    assert!(database.record_policy_decision(&duplicate).await.is_err());
    assert_eq!(acknowledgement_rows(&fixture).await.0, 2);

    fixture.cleanup().await.expect("cleanup");
}
