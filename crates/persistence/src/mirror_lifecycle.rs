//! Durable quota admission and immutable terminal evidence for mirror lifecycle runs.

use ratatoskr_vault_core::error::{FailureClass, VaultError};
use ratatoskr_vault_core::mirror::{MirrorCheckpoint, MirrorOperation, MirrorResult};
use uuid::Uuid;

use crate::Database;

/// The durable answer to a quota-reservation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaReservationOutcome {
    /// Capacity was atomically reserved for the run.
    Reserved,
    /// The requested reservation would exceed a finite per-mirror or global budget.
    QuotaExceeded,
}

type ResultFields = (&'static str, Option<&'static str>, Option<i64>, Option<i64>);

impl Database {
    /// Creates the target's identifier-derived mirror row if it is not already present.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when the database cannot write the row.
    pub async fn ensure_mirror(
        &self,
        target_id: Uuid,
        storage_path: &str,
    ) -> Result<(), VaultError> {
        sqlx::query(
            "insert into git_vault.mirrors
                 (mirror_id, target_id, status, storage_path, created_at, updated_at)
             values ($1, $2, 'initializing', $3, now(), now())
             on conflict (target_id) do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(target_id)
        .bind(storage_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(storage_failure)
    }

    /// Records a verified mirror observation after immutable run evidence was written.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when the observation cannot be written.
    pub async fn record_verified_mirror(
        &self,
        target_id: Uuid,
        bytes_on_disk: u64,
    ) -> Result<(), VaultError> {
        let bytes = checked_i64(bytes_on_disk)?;
        sqlx::query(
            "update git_vault.mirrors
             set status = 'ready', fsck_result = 'ok', bytes_on_disk = $2,
                 last_fetch_at = now(), updated_at = now()
             where target_id = $1",
        )
        .bind(target_id)
        .bind(bytes)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(storage_failure)
    }

    /// Marks the working mirror degraded when an integrity check fails, retaining prior bytes.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when the mirror row cannot be updated.
    pub async fn mark_mirror_degraded(&self, target_id: Uuid) -> Result<(), VaultError> {
        sqlx::query(
            "update git_vault.mirrors
             set status = 'degraded', fsck_result = 'failed', updated_at = now()
             where target_id = $1",
        )
        .bind(target_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(storage_failure)
    }

    /// Atomically reserves projected bytes before Git work starts.
    ///
    /// A transaction-scoped advisory lock serializes the global sum. That makes the read-check-
    /// insert sequence safe across workers without introducing an arbitrary singleton row. A
    /// refusal writes nothing, so it cannot mutate another admitted run's reservation.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] for an unrepresentable byte value or database
    /// failure. A budget refusal is [`QuotaReservationOutcome::QuotaExceeded`].
    pub async fn reserve_mirror_quota(
        &self,
        target_id: Uuid,
        run_id: Uuid,
        requested_bytes: u64,
        per_mirror_max_bytes: u64,
        global_max_bytes: u64,
    ) -> Result<QuotaReservationOutcome, VaultError> {
        let requested = checked_i64(requested_bytes)?;
        let per_limit = checked_i64(per_mirror_max_bytes)?;
        let global_limit = checked_i64(global_max_bytes)?;
        if requested <= 0 || per_limit <= 0 || global_limit <= 0 {
            return Err(VaultError::StorageFailed);
        }

        let mut tx = self.pool.begin().await.map_err(storage_failure)?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(0x7261_7461_7661_7504_i64)
            .execute(&mut *tx)
            .await
            .map_err(storage_failure)?;

        let target_reserved: i64 = sqlx::query_scalar(
            "select coalesce(sum(reserved_bytes), 0)::bigint
             from git_vault.mirror_quota_reservations where target_id = $1",
        )
        .bind(target_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_failure)?;
        let global_reserved: i64 = sqlx::query_scalar(
            "select coalesce(sum(reserved_bytes), 0)::bigint
             from git_vault.mirror_quota_reservations",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_failure)?;

        if target_reserved.saturating_add(requested) > per_limit
            || global_reserved.saturating_add(requested) > global_limit
        {
            tx.rollback().await.map_err(storage_failure)?;
            return Ok(QuotaReservationOutcome::QuotaExceeded);
        }

        sqlx::query(
            "insert into git_vault.mirror_quota_reservations
                 (run_id, target_id, reserved_bytes, created_at)
             values ($1, $2, $3, now())",
        )
        .bind(run_id)
        .bind(target_id)
        .bind(requested)
        .execute(&mut *tx)
        .await
        .map_err(storage_failure)?;
        tx.commit().await.map_err(storage_failure)?;
        Ok(QuotaReservationOutcome::Reserved)
    }

    /// Appends a terminal run result and releases that run's capacity in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::StorageFailed`] when result conversion or either database write
    /// fails. The transaction ensures a failed append cannot release the reservation.
    pub async fn record_mirror_run(
        &self,
        target_id: Uuid,
        run_id: Uuid,
        operation: MirrorOperation,
        result: MirrorResult,
    ) -> Result<(), VaultError> {
        let (outcome, checkpoint, object_count, bytes_on_disk) = result_fields(result)?;
        let mut tx = self.pool.begin().await.map_err(storage_failure)?;
        sqlx::query(
            "insert into git_vault.mirror_lifecycle_runs
                 (run_id, target_id, operation, outcome, failure_class, checkpoint,
                  object_count, bytes_on_disk, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, now())",
        )
        .bind(run_id)
        .bind(target_id)
        .bind(operation_name(operation))
        .bind(outcome)
        .bind(result.failure_class().map(FailureClass::code))
        .bind(checkpoint)
        .bind(object_count)
        .bind(bytes_on_disk)
        .execute(&mut *tx)
        .await
        .map_err(storage_failure)?;
        sqlx::query("delete from git_vault.mirror_quota_reservations where run_id = $1")
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_failure)?;
        tx.commit().await.map_err(storage_failure)
    }
}

fn checked_i64(value: u64) -> Result<i64, VaultError> {
    i64::try_from(value).map_err(|_| VaultError::StorageFailed)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err passes ownership directly and the error is emitted only on the diagnostic channel"
)]
fn storage_failure(error: sqlx::Error) -> VaultError {
    tracing::warn!(%error, "mirror lifecycle persistence query failed");
    VaultError::StorageFailed
}

fn operation_name(operation: MirrorOperation) -> &'static str {
    match operation {
        MirrorOperation::InitialClone => "clone",
        MirrorOperation::Fetch => "fetch",
    }
}

fn result_fields(result: MirrorResult) -> Result<ResultFields, VaultError> {
    match result {
        MirrorResult::Succeeded {
            object_count,
            bytes_on_disk,
        } => Ok((
            "succeeded",
            None,
            Some(checked_i64(object_count)?),
            Some(checked_i64(bytes_on_disk)?),
        )),
        MirrorResult::QuotaRefused => Ok(("quota_refused", None, None, None)),
        MirrorResult::Interrupted { checkpoint } => {
            Ok(("interrupted", Some(checkpoint_name(checkpoint)), None, None))
        }
        MirrorResult::IntegrityFailed => Ok(("integrity_failed", None, None, None)),
        MirrorResult::Failed { .. } => Ok(("failed", None, None, None)),
    }
}

fn checkpoint_name(checkpoint: MirrorCheckpoint) -> &'static str {
    match checkpoint {
        MirrorCheckpoint::ClonePending => "clone_pending",
        MirrorCheckpoint::FetchPending => "fetch_pending",
    }
}
