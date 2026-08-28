//! The Rust transition table and whatever the database enforces must agree on every ordered
//! pair of the status vocabulary.
//!
//! One fresh target row per pair: a successful update moves the row's status, so reusing one
//! row would make later pairs compare against the wrong observed state once a guard lands.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use ratatoskr_vault_core::target_state::{TargetStatus, Transition};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

/// Every ordered pair over the vocabulary produces the same verdict in Rust and in the
/// database: [`Transition::is_legal`] accepts exactly the updates the schema allows.
#[tokio::test]
async fn application_and_database_transitions_agree() {
    assert_eq!(
        TargetStatus::ALL.len(),
        12,
        "deleted must be in the closed vocabulary"
    );
    assert!(
        TargetStatus::ALL
            .iter()
            .any(|status| status.as_str() == "deleted"),
        "terminal deleted status must be represented by the application"
    );
    let fixture = TestDatabase::create()
        .await
        .expect("a disposable database with the schema applied");
    let mut disagreements = Vec::new();

    for &from in &TargetStatus::ALL {
        for &to in &TargetStatus::ALL {
            if matches!(
                (from, to),
                (TargetStatus::Excluded, TargetStatus::Deleting)
                    | (TargetStatus::Deleting, TargetStatus::Deleted)
            ) {
                // These adjacent pairs additionally require durable deletion evidence and are
                // covered by `transitions::deletion_transitions_require_stage_evidence`.
                continue;
            }
            let target_id = Uuid::now_v7();
            sqlx::query(
                "insert into git_vault.targets
                     (target_id, provider, external_repository_id, status, created_at, updated_at)
                 values ($1, 'github', $2, $3, now(), now())",
            )
            .bind(target_id)
            .bind(format!("agreement-walk-{}-{}", from.as_str(), to.as_str()))
            .bind(from.as_str())
            .execute(fixture.pool())
            .await
            .expect("the fixture insert must run");

            let accepted = fixture
                .database
                .set_target_status(target_id, to)
                .await
                .is_ok();
            // The full machine model: same-status writes are annotations the schema always
            // permits (design D2); distinct moves answer to the transition matrix alone.
            let machine_allows = from == to || Transition::is_legal(from, to);
            if accepted != machine_allows {
                disagreements.push(format!("{} -> {}", from.as_str(), to.as_str()));
            }
        }
    }

    assert!(
        disagreements.is_empty(),
        "the database and the transition table disagree on:\n{}",
        disagreements.join("\n")
    );

    fixture.cleanup().await.expect("cleanup");
}
