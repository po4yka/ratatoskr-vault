//! The `git_vault` schema: applied from one embedded file, idempotently, with state machines the
//! database itself refuses to violate. Spec: `git-vault-schema`.
//!
//! These tests talk to a real `PostgreSQL` on purpose. The queries are runtime-checked (no
//! compile-time macros), so this suite is what verifies them; a suite that silently skipped
//! without a database would prove nothing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_persistence::Database;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

/// The sixteen tables `docs/ARCHITECTURE.md` section 4 names, and no others are required here.
const REQUIRED_TABLES: [&str; 16] = [
    "targets",
    "desired_state_revisions",
    "mirrors",
    "sync_runs",
    "snapshots",
    "snapshot_artifacts",
    "lfs_snapshot_objects",
    "manifests",
    "integrity_checks",
    "restore_drills",
    "retention_policies",
    "tombstones",
    "storage_locations",
    "collector_runs",
    "outbox",
    "inbox",
];

async fn test_database() -> TestDatabase {
    TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied")
}

/// A fresh database receives the whole schema from the embedded file.
#[tokio::test]
async fn a_fresh_database_receives_every_named_table() {
    let fixture = test_database().await;

    for table in REQUIRED_TABLES {
        let present: Option<String> =
            sqlx::query_scalar(&format!("select to_regclass('git_vault.{table}')::text"))
                .fetch_one(fixture.pool())
                .await
                .expect("the catalogue query must run");
        assert!(
            present.is_some(),
            "git_vault.{table} must exist after apply"
        );
    }

    fixture.cleanup().await.expect("cleanup");
}

/// Applying the schema to a database that already has it succeeds and changes nothing.
#[tokio::test]
async fn a_second_apply_succeeds_and_changes_nothing() {
    let fixture = test_database().await;

    let objects_before: i64 = sqlx::query_scalar(
        "select count(*) from pg_class c
         join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'git_vault'",
    )
    .fetch_one(fixture.pool())
    .await
    .expect("the catalogue count must run");

    fixture
        .database
        .apply_schema()
        .await
        .expect("a second apply must be idempotent");

    let objects_after: i64 = sqlx::query_scalar(
        "select count(*) from pg_class c
         join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'git_vault'",
    )
    .fetch_one(fixture.pool())
    .await
    .expect("the catalogue count must run");

    assert_eq!(
        objects_before, objects_after,
        "a re-apply must create nothing"
    );

    fixture.cleanup().await.expect("cleanup");
}

/// The state vocabulary is enforced by the database: a status outside the target state machine is
/// refused by a CHECK constraint, not by application code that a second writer might bypass.
#[tokio::test]
async fn a_target_status_outside_the_state_machine_is_refused() {
    let fixture = test_database().await;

    let result = sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', '1234567', 'totally-fine-i-promise', now(), now())",
    )
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await;

    let error = result.expect_err("an out-of-vocabulary state must be refused");
    assert!(
        matches!(
            error.as_database_error(),
            Some(db) if db.is_check_violation()
        ),
        "expected a CHECK violation, got {error}"
    );

    fixture.cleanup().await.expect("cleanup");
}

/// A valid target row goes in: the happy path of the same vocabulary.
#[tokio::test]
async fn a_target_in_the_requested_state_is_accepted() {
    let fixture = test_database().await;

    let target_id = Uuid::now_v7();
    let inserted = sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', '1234567', 'requested', now(), now())",
    )
    .bind(target_id)
    .execute(fixture.pool())
    .await
    .expect("a state inside the vocabulary must be accepted");
    assert_eq!(inserted.rows_affected(), 1);

    fixture.cleanup().await.expect("cleanup");
}

/// The readiness probe is a real round trip, not a pool-state inspection.
#[tokio::test]
async fn ping_answers_on_a_live_database() {
    let fixture = test_database().await;

    fixture.database.ping().await.expect("ping must answer");

    fixture.cleanup().await.expect("cleanup");
}

/// `connect` refuses an unreachable server instead of handing back a lazy pool that fails later.
#[tokio::test]
async fn connect_refuses_an_unreachable_server() {
    use ratatoskr_vault_core::config::DatabaseConfig;
    use secrecy::SecretString;

    let config = DatabaseConfig {
        url: SecretString::from("postgres://vault:nobody@127.0.0.1:59999/vault".to_owned()),
        max_connections: 2,
        acquire_timeout_seconds: 1,
    };
    let result = Database::connect(&config).await;
    assert!(
        result.is_err(),
        "a closed port must fail at connect time, not at first use"
    );
}

#[tokio::test]
async fn collector_and_target_schema_is_closed_to_approved_lfs_and_wiki_shapes() {
    let fixture = test_database().await;

    let target_columns: Vec<String> = sqlx::query_scalar(
        "select column_name from information_schema.columns
         where table_schema = 'git_vault' and table_name = 'targets'
           and column_name in ('target_kind', 'parent_target_id')
         order by column_name",
    )
    .fetch_all(fixture.pool())
    .await
    .expect("target shape inventory must query");
    assert_eq!(
        target_columns,
        ["parent_target_id", "target_kind"],
        "repository/wiki relationship columns must exist"
    );

    let lfs_objects: Option<String> =
        sqlx::query_scalar("select to_regclass('git_vault.lfs_snapshot_objects')::text")
            .fetch_one(fixture.pool())
            .await
            .expect("LFS object table inventory must query");
    assert!(
        lfs_objects.is_some(),
        "snapshot LFS object links must exist"
    );

    let collector_constraint: String = sqlx::query_scalar(
        "select pg_get_constraintdef(oid)
         from pg_constraint
         where conname = 'collector_runs_collector_is_known'",
    )
    .fetch_one(fixture.pool())
    .await
    .expect("collector vocabulary constraint must exist");
    assert!(collector_constraint.contains("'git_lfs'::text"));
    assert!(collector_constraint.contains("'wiki'::text"));
    for forbidden in [
        "releases",
        "issues",
        "pull_requests",
        "discussions",
        "settings",
    ] {
        assert!(
            !collector_constraint.contains(forbidden),
            "unapproved collector {forbidden} remains executable in the schema"
        );
    }

    fixture.cleanup().await.expect("cleanup");
}

/// The current schema carries the whole retention/deletion evidence model; no later migration is
/// expected to fill in a partial foundation.
#[tokio::test]
async fn current_schema_has_retention_deletion_evidence() {
    let fixture = test_database().await;
    let required_tables = [
        "snapshot_pins",
        "retention_evaluations",
        "retention_candidates",
        "deletion_plans",
        "deletion_stage_attempts",
        "physical_object_claims",
        "retention_audit",
    ];
    let mut missing = Vec::new();
    for table in required_tables {
        let present: Option<String> =
            sqlx::query_scalar(&format!("select to_regclass('git_vault.{table}')::text"))
                .fetch_one(fixture.pool())
                .await
                .expect("retention table catalogue query must run");
        if present.is_none() {
            missing.push(table);
        }
    }

    let policy_columns: Vec<String> = sqlx::query_scalar(
        "select column_name from information_schema.columns
         where table_schema = 'git_vault' and table_name = 'retention_policies'
           and column_name in ('minimum_age_seconds', 'grace_seconds',
                               'keep_last_restorable')
         order by column_name",
    )
    .fetch_all(fixture.pool())
    .await
    .expect("retention policy columns must query");
    let guard_functions: Vec<String> = sqlx::query_scalar(
        "select proname from pg_proc
         join pg_namespace on pg_namespace.oid = pg_proc.pronamespace
         where pg_namespace.nspname = 'git_vault'
           and proname in ('guard_snapshot_pin_mutation', 'guard_tombstone_mutation',
                           'guard_deletion_plan_mutation', 'guard_deletion_stage_attempt')
         order by proname",
    )
    .fetch_all(fixture.pool())
    .await
    .expect("retention guard inventory must query");

    assert!(missing.is_empty(), "missing retention tables: {missing:?}");
    assert_eq!(
        policy_columns,
        [
            "grace_seconds",
            "keep_last_restorable",
            "minimum_age_seconds"
        ]
    );
    assert_eq!(guard_functions.len(), 4);

    fixture.cleanup().await.expect("cleanup");
}
