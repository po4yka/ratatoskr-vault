//! The `PostgreSQL` pool Vault owns, the `git_vault` schema embedded in the binary, and the
//! readiness probe.
//!
//! Scope. This crate owns the `git_vault` schema and nothing else. Vault writes only its own
//! schema and reads none beside it; keeping one type as the only route to a connection is what
//! keeps that true over time.
//!
//! The schema is ONE file, `schema.sql` at the repository root, not a migration directory and not
//! a numbered ledger. No database holds data that has to survive a schema change, so an
//! incremental history buys nothing and costs a rule that an applied file can never be edited. A
//! schema change edits `schema.sql` in place.

#[cfg(feature = "test-support")]
pub mod test_support;

mod mirror_lifecycle;
mod replication;
mod restore_verification;
mod snapshot;

use std::time::Duration;

use ratatoskr_vault_core::delivery::ValidatedDelivery;
use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_core::target_state::TargetStatus;
use secrecy::ExposeSecret as _;
use sqlx::error::DatabaseError;
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

pub use crate::mirror_lifecycle::QuotaReservationOutcome;
pub use crate::replication::{
    DueReplicationUnit, ReplicaTargetObservation, StoredReplicaPlacement,
};
pub use crate::restore_verification::{
    EvidenceOutcome, StoredRestoreDrillReport, StoredRestoreSource, StoredVerificationReport,
    VerificationScheduleRecord,
};
pub use crate::snapshot::{SnapshotParent, SnapshotSource};

/// The schema, embedded at compile time.
///
/// Embedded rather than read from disk so a deployed binary cannot be paired with a different
/// schema than the one it was built against. `include_str!` makes the file a build input, so
/// editing it rebuilds this crate — which is the whole of the staleness protection.
const SCHEMA: &str = include_str!("../../../schema.sql");

/// The advisory-lock key `apply_schema` holds while it decides and applies.
///
/// One arbitrary but fixed 64-bit value; `PostgreSQL` advisory locks are a namespace of integers
/// with no meaning of their own, and nothing else in this system takes one. The lock still
/// matters with one process per deployment: a restart that overlaps the previous process's grace
/// window is two processes, for a few seconds, and both call this method.
const SCHEMA_LOCK: i64 = 0x7261_7461_7661_7501;

/// A failure in the pool or a query.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PersistenceError {
    /// The pool could not be created, or a connection could not be acquired.
    #[error("the database connection could not be established")]
    Connect(#[source] sqlx::Error),

    /// The schema could not be applied.
    #[error("the database schema could not be applied")]
    Schema(#[source] sqlx::Error),

    /// A query failed.
    #[error("a database query failed")]
    Query(#[source] sqlx::Error),
}

/// The pool, and the only handle through which Vault reaches `PostgreSQL`.
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Create the pool and verify it can serve one connection.
    ///
    /// The verification is not ceremony: it is possible to hold a pool whose credentials are
    /// wrong, and finding that out on the first request rather than at startup is how a deployment
    /// reports itself healthy and then fails every call.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Connect`] if the URL is unusable or the server refuses the connection
    /// within the configured acquire timeout.
    pub async fn connect(
        config: &ratatoskr_vault_core::config::DatabaseConfig,
    ) -> Result<Self, PersistenceError> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_seconds))
            .test_before_acquire(true)
            .connect(config.url.expose_secret())
            .await
            .map_err(PersistenceError::Connect)?;

        Ok(Self { pool })
    }

    /// Apply [`SCHEMA`] to a database that does not have it yet.
    ///
    /// Idempotent, and safe to run while another process is still holding connections. One
    /// transaction does all three things: it takes a `PostgreSQL` advisory lock, asks whether
    /// `git_vault` exists, and applies the file only if it does not. The lock is
    /// transaction-scoped, so it is released by the commit and by a panic alike, and a second
    /// process that arrives during a restart waits for the first, then sees the schema and does
    /// nothing.
    ///
    /// `PostgreSQL` DDL is transactional, so a file that fails halfway leaves the database exactly
    /// as it was rather than half-applied. The presence check is therefore an honest question:
    /// either every object in the file is there or none of it is.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Schema`] if the lock cannot be taken, the catalogue cannot be read, or
    /// a statement in the file fails.
    pub async fn apply_schema(&self) -> Result<(), PersistenceError> {
        let mut transaction = self.pool.begin().await.map_err(PersistenceError::Schema)?;
        lock_and_apply(&mut transaction)
            .await
            .map_err(PersistenceError::Schema)?;
        transaction.commit().await.map_err(PersistenceError::Schema)
    }

    /// Answer whether the database is usable right now.
    ///
    /// Deliberately a round trip and not a pool-state inspection: a pool with idle connections to
    /// a server that is refusing queries looks healthy from the inside.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Query`] if the round trip fails or times out.
    pub async fn ping(&self) -> Result<(), PersistenceError> {
        sqlx::query("select 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(PersistenceError::Query)
    }

    /// Sets one target's status through whatever the database enforces.
    ///
    /// Deliberately no transition logic of its own: the guard is database-side (`schema.sql`),
    /// so this op and a manual UPDATE meet exactly the same rule.
    ///
    /// # Errors
    ///
    /// [`VaultError::IllegalTransition`] naming both statuses when the guard refuses the move;
    /// [`VaultError::StorageFailed`] when anything else fails, with the underlying error logged.
    pub async fn set_target_status(
        &self,
        target_id: Uuid,
        status: TargetStatus,
    ) -> Result<(), VaultError> {
        sqlx::query("update git_vault.targets set status = $2 where target_id = $1")
            .bind(target_id)
            .bind(status.as_str())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|err| classify_status_update_failure(&err))
    }

    /// Applies one governed transition to a target in one transaction.
    ///
    /// The governing revision lands as append-only evidence, the target row is taken with
    /// `SELECT ... FOR UPDATE`, the status write faces the database guard like any other writer,
    /// and the state-changed event joins the same commit only when the status truly changed:
    /// same-status writes are annotations, never events.
    ///
    /// # Errors
    ///
    /// [`VaultError::InvalidDelivery`] naming the rejected field when the governing record lacks
    /// a usable revision or correlation id, or names an unknown target;
    /// [`VaultError::IllegalTransition`] carrying both statuses when the guard refuses the move,
    /// with nothing committed; [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn apply_transition(
        &self,
        target_id: Uuid,
        to_status: TargetStatus,
        governing: &ValidatedDelivery,
    ) -> Result<(), VaultError> {
        let (policy_revision, correlation_id) = governed_inputs(governing)?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| classify_status_update_failure(&err))?;

        sqlx::query(
            "insert into git_vault.desired_state_revisions
                 (revision_id, target_id, policy_revision, preservation_level, pinned,
                  include_wiki, include_releases, include_issues, offsite_required,
                  correlation_id, received_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
             on conflict do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(target_id)
        .bind(policy_revision)
        .bind(&governing.preservation_level)
        .bind(governing.pinned.unwrap_or(false))
        .bind(governing.include_wiki.unwrap_or(false))
        .bind(governing.include_releases.unwrap_or(false))
        .bind(governing.include_issues.unwrap_or(false))
        .bind(governing.offsite_required.unwrap_or(false))
        .bind(correlation_id)
        .execute(&mut *tx)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;

        let current: Option<String> = sqlx::query_scalar(
            "select status from git_vault.targets where target_id = $1 for update",
        )
        .bind(target_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;
        let Some(from_status) = current else {
            return Err(VaultError::InvalidDelivery { field: "target_id" });
        };

        sqlx::query(
            "update git_vault.targets set status = $2, updated_at = now() where target_id = $1",
        )
        .bind(target_id)
        .bind(to_status.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;

        if from_status != to_status.as_str() {
            sqlx::query(
                "insert into git_vault.outbox
                     (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
                 values ($1, 'vault.target.state_changed.v1', 'target', $2,
                         jsonb_build_object('target_id', $2, 'from_status', $3,
                                            'to_status', $4, 'policy_revision', $5,
                                            'correlation_id', $6),
                         now())",
            )
            .bind(Uuid::now_v7())
            .bind(target_id)
            .bind(&from_status)
            .bind(to_status.as_str())
            .bind(policy_revision)
            .bind(correlation_id)
            .execute(&mut *tx)
            .await
            .map_err(|err| classify_status_update_failure(&err))?;
        }

        tx.commit()
            .await
            .map_err(|err| classify_status_update_failure(&err))
    }

    /// Ingests one delivered desired-state message in one transaction: the inbox slot is
    /// claimed first, so a redelivered `(source, message_id)` pair is refused before any state
    /// work happens; then the target row is ensured on first sight, and the governing record
    /// lands as append-only revision evidence.
    ///
    /// # Errors
    ///
    /// [`VaultError::DuplicateDelivery`] when the pair was consumed already, leaving nothing
    /// written; [`VaultError::InvalidDelivery`] naming the rejected field when the governing
    /// record lacks a usable revision or correlation id; [`VaultError::StorageFailed`] for
    /// infrastructure failures, logged.
    pub async fn ingest_delivery(
        &self,
        provider: &str,
        external_repository_id: &str,
        source: &str,
        message_id: Uuid,
        delivery: &ValidatedDelivery,
    ) -> Result<Uuid, VaultError> {
        let (policy_revision, correlation_id) = governed_inputs(delivery)?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| classify_status_update_failure(&err))?;

        let claim = sqlx::query(
            "insert into git_vault.inbox (message_id, source, consumed_at) values ($1, $2, now())",
        )
        .bind(message_id)
        .bind(source)
        .execute(&mut *tx)
        .await;
        if let Err(err) = claim {
            if err
                .as_database_error()
                .is_some_and(DatabaseError::is_unique_violation)
            {
                return Err(VaultError::DuplicateDelivery);
            }
            return Err(classify_status_update_failure(&err));
        }

        let created: Option<Uuid> = sqlx::query_scalar(
            "insert into git_vault.targets
                 (target_id, provider, external_repository_id, status, created_at, updated_at)
             values ($1, $2, $3, 'requested', now(), now())
             on conflict (provider, external_repository_id) do nothing
             returning target_id",
        )
        .bind(Uuid::now_v7())
        .bind(provider)
        .bind(external_repository_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;

        let target_id = match created {
            Some(id) => {
                // First sight is enrollment (design D6): the insert of the target row IS the
                // status write, so its event joins the same transaction as null -> requested.
                sqlx::query(
                    "insert into git_vault.outbox
                         (event_id, event_type, aggregate_type, aggregate_id, payload, created_at)
                     values ($1, 'vault.target.state_changed.v1', 'target', $2,
                             jsonb_build_object('target_id', $2, 'from_status', null,
                                                'to_status', 'requested',
                                                'policy_revision', $3,
                                                'correlation_id', $4),
                             now())",
                )
                .bind(Uuid::now_v7())
                .bind(id)
                .bind(policy_revision)
                .bind(correlation_id)
                .execute(&mut *tx)
                .await
                .map_err(|err| classify_status_update_failure(&err))?;
                id
            }
            None => sqlx::query_scalar(
                "select target_id from git_vault.targets
                     where provider = $1 and external_repository_id = $2 for update",
            )
            .bind(provider)
            .bind(external_repository_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|err| classify_status_update_failure(&err))?
            .ok_or(VaultError::InvalidDelivery { field: "target_id" })?,
        };

        sqlx::query(
            "insert into git_vault.desired_state_revisions
                 (revision_id, target_id, policy_revision, preservation_level, pinned,
                  include_wiki, include_releases, include_issues, offsite_required,
                  correlation_id, received_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
             on conflict do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(target_id)
        .bind(policy_revision)
        .bind(&delivery.preservation_level)
        .bind(delivery.pinned.unwrap_or(false))
        .bind(delivery.include_wiki.unwrap_or(false))
        .bind(delivery.include_releases.unwrap_or(false))
        .bind(delivery.include_issues.unwrap_or(false))
        .bind(delivery.offsite_required.unwrap_or(false))
        .bind(correlation_id)
        .execute(&mut *tx)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;

        tx.commit()
            .await
            .map_err(|err| classify_status_update_failure(&err))?;

        Ok(target_id)
    }

    /// Reads the one revision that governs a target's reconciliation.
    ///
    /// Governance is read-time and number-based (design D3): the maximum delivered revision
    /// rules regardless of arrival order, while every stale row stays as evidence.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn governing_policy(
        &self,
        target_id: Uuid,
    ) -> Result<Option<GoverningPolicy>, VaultError> {
        let row: Option<GoverningRow> = sqlx::query_as(
            "select target_id, policy_revision, preservation_level, pinned,
                    include_wiki, include_releases, include_issues, offsite_required,
                    correlation_id
             from git_vault.desired_state_revisions
             where target_id = $1
             order by policy_revision desc
             limit 1",
        )
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| classify_status_update_failure(&err))?;

        Ok(row.map(
            |(
                target_id,
                policy_revision,
                preservation_level,
                pinned,
                include_wiki,
                include_releases,
                include_issues,
                offsite_required,
                correlation_id,
            )| {
                GoverningPolicy {
                    target_id,
                    policy_revision,
                    preservation_level,
                    pinned,
                    include_wiki,
                    include_releases,
                    include_issues,
                    offsite_required,
                    correlation_id,
                }
            },
        ))
    }

    /// Reads a target's currently stored status through the closed vocabulary.
    ///
    /// [`None`] when no such target row exists.
    ///
    /// # Errors
    ///
    /// [`VaultError::StorageFailed`] for infrastructure failures, logged.
    pub async fn observed_status(
        &self,
        target_id: Uuid,
    ) -> Result<Option<TargetStatus>, VaultError> {
        let status: Option<String> =
            sqlx::query_scalar("select status from git_vault.targets where target_id = $1")
                .bind(target_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|err| classify_status_update_failure(&err))?;

        let Some(status) = status else {
            return Ok(None);
        };
        if let Some(known) = TargetStatus::ALL
            .iter()
            .copied()
            .find(|known| known.as_str() == status)
        {
            Ok(Some(known))
        } else {
            tracing::warn!(
                status = %status,
                "a stored target status is outside the vocabulary"
            );
            Err(VaultError::StorageFailed)
        }
    }

    /// The pool, for the crate that owns the schema's queries.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Close the pool and wait for checked-out connections to be returned.
    ///
    /// Called from the shutdown sequence after the listener stops accepting, so an in-flight
    /// request keeps its connection through the grace window.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// The body of [`Database::apply_schema`], on one connection so the lock and the apply share a
/// session.
///
/// A free function taking the connection by its named type: async trait methods have to prove
/// their futures are `Send`, and that proof needs the executor's lifetime pinned rather than
/// inferred at the call site (rust-lang/rust#100013, seen as "implementation of `Executor` is not
/// general enough").
///
/// The file goes through `Executor::execute` and NOT `sqlx::raw_sql`, which trips the same bound.
/// Both send the string over the simple query protocol, which runs every statement in it;
/// `execute` folds the per-statement results into one.
async fn lock_and_apply(connection: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(SCHEMA_LOCK)
        .execute(&mut *connection)
        .await?;

    // The one schema the file creates. Under the lock, its absence means the file has never been
    // applied to this database.
    let present: Option<String> = sqlx::query_scalar("select to_regnamespace('git_vault')::text")
        .fetch_one(&mut *connection)
        .await?;

    if present.is_none() {
        sqlx::Executor::execute(connection, SCHEMA).await?;
    }

    Ok(())
}

/// The SQLSTATE the target guard raises for an illegal move (`schema.sql`).
const GUARD_ERRCODE: &str = "VLT01";

/// Maps one failed status update onto the typed error set.
///
/// The guard's refusal message is ours to define (`illegal target transition <from> -> <to>`),
/// so both statuses travel with the error. Everything else is storage-level detail that stays
/// on the telemetry channel rather than inside the value.
fn classify_status_update_failure(err: &sqlx::Error) -> VaultError {
    let refusal = err
        .as_database_error()
        .filter(|db| db.code().as_deref() == Some(GUARD_ERRCODE));
    if let Some(db) = refusal {
        let parsed = db
            .message()
            .strip_prefix("illegal target transition ")
            .and_then(|rest| rest.split_once(" -> "));
        if let Some((from, to)) = parsed {
            return VaultError::IllegalTransition {
                from: from.to_owned(),
                to: to.to_owned(),
            };
        }
        tracing::warn!(
            guard_message = db.message(),
            "the target guard refused a move in an unexpected message shape"
        );
        return VaultError::StorageFailed;
    }
    tracing::warn!(error = %err, "a target status update failed");
    VaultError::StorageFailed
}

/// The revision number and correlation id every governed record must carry, checked once at
/// the persistence edge.
fn governed_inputs(delivery: &ValidatedDelivery) -> Result<(i64, Uuid), VaultError> {
    let policy_revision = delivery
        .policy_revision
        .ok_or(VaultError::InvalidDelivery {
            field: "policy_revision",
        })?;
    let policy_revision =
        i64::try_from(policy_revision).map_err(|_| VaultError::InvalidDelivery {
            field: "policy_revision",
        })?;
    let correlation_id: Uuid =
        delivery
            .correlation_id
            .parse()
            .map_err(|_| VaultError::InvalidDelivery {
                field: "correlation_id",
            })?;
    Ok((policy_revision, correlation_id))
}

/// The one revision that governs a target's reconciliation, as planning may consume it.
///
/// The flags stay plain bools because the database columns are NOT NULL; they mirror the
/// desired-state contract's independent toggles one-to-one.
#[expect(
    clippy::struct_excessive_bools,
    reason = "five independent contract flags map straight onto five independent columns"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct GoverningPolicy {
    /// The target this record governs.
    pub target_id: Uuid,
    /// The governing revision number.
    pub policy_revision: i64,
    /// Preservation level exactly as recorded.
    pub preservation_level: String,
    /// Whether the target resists automatic exclusion.
    pub pinned: bool,
    /// Whether the wiki repository is included.
    pub include_wiki: bool,
    /// Whether releases and their assets are included.
    pub include_releases: bool,
    /// Whether issues and comments are included.
    pub include_issues: bool,
    /// Whether an off-host copy is required.
    pub offsite_required: bool,
    /// The correlation id of the governing delivery.
    pub correlation_id: Uuid,
}

/// One governing-revision row straight from the query; positional order must match the SELECT.
type GoverningRow = (Uuid, i64, String, bool, bool, bool, bool, bool, Uuid);
