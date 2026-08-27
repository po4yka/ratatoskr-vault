//! Wiki sibling enrollment and append-only discovery evidence.

#![allow(clippy::expect_used, reason = "test assertions")]

use ratatoskr_vault_persistence::WikiDiscovery;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

#[tokio::test]
async fn repeated_present_wiki_discovery_enrolls_one_related_target() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let parent = seed_parent(&fixture).await;

    let first = fixture
        .database
        .record_wiki_discovery(parent, WikiDiscovery::Present)
        .await
        .expect("first discovery");
    let repeated = fixture
        .database
        .record_wiki_discovery(parent, WikiDiscovery::Present)
        .await
        .expect("repeated discovery");

    assert_eq!(first.child_target_id, repeated.child_target_id);
    let child_count: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.targets
         where target_kind = 'wiki' and parent_target_id = $1",
    )
    .bind(parent)
    .fetch_one(fixture.pool())
    .await
    .expect("child count");
    assert_eq!(child_count, 1);
    let derived: (String, bool, bool, bool) = sqlx::query_as(
        "select preservation_level, include_wiki, include_releases, include_issues
         from git_vault.desired_state_revisions where target_id = $1",
    )
    .bind(first.child_target_id.expect("wiki child"))
    .fetch_one(fixture.pool())
    .await
    .expect("derived policy");
    assert_eq!(derived, ("git_mirror".to_owned(), false, false, false));
    let evidence_count: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.collector_runs
         where target_id = $1 and collector = 'wiki' and outcome = 'complete'",
    )
    .bind(parent)
    .fetch_one(fixture.pool())
    .await
    .expect("append-only evidence count");
    assert_eq!(evidence_count, 2);
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn absent_and_failed_discovery_are_distinct_and_never_enroll_a_child() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let parent = seed_parent(&fixture).await;
    fixture
        .database
        .record_wiki_discovery(parent, WikiDiscovery::Absent)
        .await
        .expect("confirmed absence");
    fixture
        .database
        .record_wiki_discovery(
            parent,
            WikiDiscovery::Failed {
                failure_class: "auth".to_owned(),
            },
        )
        .await
        .expect("failed probe evidence");

    let outcomes: Vec<(String, Option<String>)> = sqlx::query_as(
        "select outcome, failure_class from git_vault.collector_runs
         where target_id = $1 and collector = 'wiki' order by ran_at, collector_run_id",
    )
    .bind(parent)
    .fetch_all(fixture.pool())
    .await
    .expect("discovery evidence");
    assert_eq!(
        outcomes,
        vec![
            ("absent".to_owned(), None),
            ("failed".to_owned(), Some("auth".to_owned()))
        ]
    );
    let children: i64 =
        sqlx::query_scalar("select count(*) from git_vault.targets where parent_target_id = $1")
            .bind(parent)
            .fetch_one(fixture.pool())
            .await
            .expect("child count");
    assert_eq!(children, 0);
    fixture.cleanup().await.expect("cleanup");
}

async fn seed_parent(fixture: &TestDatabase) -> Uuid {
    let parent = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', 'fixture-repository', 'ready', now(), now())",
    )
    .bind(parent)
    .execute(fixture.pool())
    .await
    .expect("parent target");
    sqlx::query(
        "insert into git_vault.desired_state_revisions
             (revision_id, target_id, policy_revision, preservation_level, pinned,
              include_wiki, include_releases, include_issues, offsite_required,
              correlation_id, received_at)
         values ($1, $2, 1, 'complete_archive', true, true, false, false, true, $3, now())",
    )
    .bind(Uuid::now_v7())
    .bind(parent)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("parent policy");
    parent
}
