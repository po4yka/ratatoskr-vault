//! The `ratatoskr-vault` deployable.
//!
//! Typed configuration, telemetry, the admin plane, the `git_vault` schema, and, when a bus is
//! configured, the policy lane (XR-021): the consumer of GitHub's desired backup policy and the
//! relay of Vault's acknowledgement. Mirror execution for the enrolled targets is a later plan
//! item and is deliberately absent here.
//!
//! This binary owns the schema: it applies `schema.sql` to a fresh database at startup. There is
//! one process per deployment and one schema owner, which is what makes the advisory-locked apply
//! in `ratatoskr_vault_persistence` safe.

use std::process::ExitCode;
use std::sync::Arc;

use ratatoskr_vault::policy_bus;
use ratatoskr_vault_core::config::VaultConfig;
use ratatoskr_vault_http::{RuntimeState, Serving};

/// What this deployable contributes to the process lifecycle: connect the database it is
/// configured with, apply the schema it owns, and refuse to start when either fails.
struct VaultRoutes;

impl ratatoskr_vault_http::ServiceRoutes for VaultRoutes {
    async fn build(
        self,
        config: &VaultConfig,
        health: &Arc<RuntimeState>,
    ) -> Result<Serving, String> {
        // A database is optional at this milestone: a process without one serves its probes and
        // reports no database check. A process WITH one that cannot be reached must not start —
        // reporting ready and then failing every future request is the failure mode this refusal
        // exists to prevent.
        let database = match config.database.as_ref() {
            Some(database) => Some(
                ratatoskr_vault_persistence::Database::connect(database)
                    .await
                    .map_err(|error| {
                        format!("the database could not be reached: {error}; check RATATOSKR__DATABASE__URL")
                    })?,
            ),
            None => None,
        };

        if let Some(database) = database.as_ref() {
            database
                .apply_schema()
                .await
                .map_err(|error| format!("the git_vault schema could not be applied: {error}"))?;
        }

        // The lane needs the database it writes to; configuration refuses a bus without one
        // (exit 78), so both are present here or the lane is not configured at all. A lane that
        // cannot reach its broker or verify its durable refuses the start (exit 1) rather than
        // reporting ready beside work it cannot do.
        let tasks = match (config.bus.as_ref(), database.as_ref()) {
            (Some(bus), Some(database)) => {
                policy_bus::start(database.clone(), bus, Arc::clone(health))
                    .await
                    .map_err(|reason| format!("the policy lane could not start: {reason}"))?
            }
            _ => Vec::new(),
        };

        Ok(Serving {
            // No routes beyond the admin plane yet: the first public or worker routes arrive with
            // their plan items.
            routes: None,
            database,
            tasks,
        })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("check-config") {
        return ratatoskr_vault_http::check_config();
    }
    ratatoskr_vault_http::run(VaultRoutes).await
}
