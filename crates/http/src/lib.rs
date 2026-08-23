//! The shared HTTP harness the Vault deployable runs: `run()` and nothing else in `main`.
//!
//! - [`run`] and [`check_config`] — the whole process lifecycle, so the binary cannot drift.
//! - [`admin_router`] — liveness, readiness, metrics and version, on the operator listener only.
//! - [`RuntimeState`] — the facts readiness is computed from.
//! - [`serve`] and [`drain_and_close`] — the drain-then-close-then-flush sequence.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |---|---|
//! | `0` | Clean start and clean shutdown |
//! | `1` | Runtime startup failure: telemetry initialisation, a route builder that refused, or a listener that could not bind |
//! | `78` | `EX_CONFIG` — the configuration is unreadable or invalid; nothing was bound |

mod admin;
mod lifecycle;
mod shutdown;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use ratatoskr_vault_core::config::VaultConfig;
use ratatoskr_vault_persistence::Database;
use ratatoskr_vault_telemetry::identity;
use tokio::net::TcpListener;
use tracing::field::Empty;

pub use crate::admin::admin_router;
pub use crate::lifecycle::{Check, CheckName, CheckReason, CheckState, RuntimeState};
pub use crate::shutdown::{Served, ShutdownOutcome, drain_and_close, serve};

/// How often the database prober asks whether the dependency is still there.
///
/// Five seconds: long enough that the probe is not itself load, short enough that a readiness
/// state is never more than one scrape interval stale.
const DATABASE_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// What the deployable contributes to the process.
///
/// A trait rather than a `Router` argument, because the routes need the configuration this
/// function loads — the database URL above all — and a binary cannot build them before `run` has
/// read it. A trait rather than a closure, because the future must borrow the configuration and an
/// `async` closure that does so is not expressible without naming the lifetime anyway.
pub trait ServiceRoutes {
    /// Build the routes, or explain why the process must not start.
    ///
    /// Returning an error here is a startup failure, not a request failure: a binary that cannot
    /// reach the database it needs must refuse to report itself ready rather than serve failures
    /// forever.
    fn build(
        self,
        config: &VaultConfig,
        health: &Arc<RuntimeState>,
    ) -> impl Future<Output = Result<Serving, String>> + Send;
}

/// What the binary serves, and what it runs alongside.
#[derive(Debug, Default)]
pub struct Serving {
    /// The routes. The admin router is built by `run` itself; this carries additional routes when
    /// a milestone adds them. Empty today.
    pub routes: Option<Router>,
    /// The pool, when this deployment has one. `run` probes it for readiness and closes it after
    /// the grace window, so a binary does not have to remember to.
    pub database: Option<Database>,
    /// Background work that must stop when the process does. Aborted after the listeners close,
    /// never before.
    pub tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// The whole process lifecycle. The binary's `main` is this call and nothing else.
///
/// Sequence, in this order and no other:
///
/// 1. `ratatoskr_vault_core::config::load()` — on failure write the report to stderr and exit 78.
/// 2. `ratatoskr_vault_telemetry::init` — on failure write to stderr, exit 1. Telemetry is
///    initialised AFTER validation so an invalid `log_filter` is a configuration error, not a
///    failure inside subscriber setup where nothing can report it.
/// 3. Open `vault.startup`. Log the effective configuration at INFO (safe by type) and the
///    non-fatal warnings.
/// 4. Build the routes; bind the admin listener. On failure log at ERROR and exit 1.
/// 5. [`RuntimeState::mark_startup_complete`] — readiness flips to 200.
/// 6. Serve until SIGTERM or SIGINT, then [`drain_and_close`].
/// 7. Close the pool; `TelemetryGuard::shutdown()`; exit 0.
pub async fn run<R: ServiceRoutes>(routes: R) -> ExitCode {
    let config = match ratatoskr_vault_core::config::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{}", error.report());
            return ExitCode::from(error.exit_code());
        }
    };

    let guard = match ratatoskr_vault_telemetry::init(&config.telemetry) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!(
                "ratatoskr-vault: refusing to start; telemetry could not be initialised: {error}"
            );
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    let startup = tracing::info_span!(
        "vault.startup",
        version = identity::VERSION,
        git_sha = identity::GIT_SHA,
        duration_ms = Empty,
    );
    startup.in_scope(|| announce(&config));

    let runtime = Arc::new(RuntimeState::new());
    let metrics = guard.metrics_handle();

    let Serving {
        routes: extra_routes,
        database,
        tasks,
    } = match routes.build(&config, &runtime).await {
        Ok(built) => built,
        Err(reason) => {
            startup.in_scope(|| {
                tracing::error!(%reason, "the routes could not be built");
            });
            return ExitCode::FAILURE;
        }
    };

    let prober = start_database_prober(database.as_ref(), &runtime).await;

    let Some(server) = startup
        .in_scope(|| bind_admin_listener(&config, &runtime, metrics, extra_routes))
        .await
    else {
        return ExitCode::FAILURE;
    };

    runtime.mark_startup_complete();
    startup.record("duration_ms", startup_duration_ms(started.elapsed()));
    startup.in_scope(|| {
        tracing::info!(admin = %config.admin.bind, "startup complete");
    });
    drop(startup);

    shutdown::signal().await;
    let outcome =
        drain_and_close(&runtime, &config.shutdown, vec![server], shutdown::signal()).await;
    tracing::info!(graceful = outcome.graceful(), "the process stopped");

    if let Some(prober) = prober {
        prober.abort();
    }
    for task in tasks {
        task.abort();
    }
    if let Some(database) = database {
        // After the listener stopped accepting and the grace window closed, so an in-flight
        // request kept its connection for its whole life.
        database.close().await;
    }

    guard.shutdown();
    ExitCode::SUCCESS
}

/// `ratatoskr-vault check-config`: load and validate without binding anything; write the effective
/// configuration or the failure report; exit 0 or 78.
///
/// It exists so an environment can be validated in CI or an init container before the process is
/// allowed to start. Both outputs go to stderr: no subscriber exists yet, and the workspace
/// forbids writing to stdout so that a stray line can never be mistaken for a log record.
#[must_use]
pub fn check_config() -> ExitCode {
    match ratatoskr_vault_core::config::load() {
        Ok(config) => {
            // Safe by type: the secret members render as `[REDACTED]` however deeply nested.
            eprintln!("ratatoskr-vault: configuration is valid.\n{config:#?}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error.report());
            ExitCode::from(error.exit_code())
        }
    }
}

/// The single INFO line that says what the process actually believes, and the non-fatal warning.
/// Safe by type: `SecretString` has no `Display` and renders as `[REDACTED]`.
fn announce(config: &VaultConfig) {
    tracing::info!(config = ?config, version = identity::VERSION, "effective configuration");
    if !config.admin.bind.ip().is_loopback() {
        tracing::warn!(
            bind = %config.admin.bind,
            "the admin plane is not bound to a loopback address; it must never be published \
             through an ingress"
        );
    }
    if config.telemetry.otlp.is_none() {
        tracing::warn!(
            "no OTLP endpoint is configured; spans are created and carry real trace ids, but \
             nothing is exported"
        );
    }
}

/// Startup duration in whole milliseconds, saturating rather than panicking on an absurd clock.
fn startup_duration_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Probe the database once, then keep probing until the process stops.
///
/// The first probe happens BEFORE the listener opens, so a process never reports itself ready
/// with an unverified dependency.
async fn start_database_prober(
    database: Option<&Database>,
    runtime: &Arc<RuntimeState>,
) -> Option<tokio::task::JoinHandle<()>> {
    let database = database?;
    runtime.set_database_reachable(database.ping().await.is_ok());

    let database = database.clone();
    let runtime = Arc::clone(runtime);
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DATABASE_PROBE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            runtime.set_database_reachable(database.ping().await.is_ok());
        }
    }))
}

/// Bind the operator listener and serve the admin plane, plus any extra routes the binary built.
///
/// `None` on failure; the caller exits 1. The error is logged here, inside the startup span, so it
/// carries the same fields as every other startup record.
async fn bind_admin_listener(
    config: &VaultConfig,
    runtime: &Arc<RuntimeState>,
    metrics: metrics_exporter_prometheus::PrometheusHandle,
    extra_routes: Option<Router>,
) -> Option<Served> {
    let admin = match TcpListener::bind(config.admin.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(bind = %config.admin.bind, %error, "the admin listener could not bind");
            return None;
        }
    };

    let metrics_handle = metrics;
    let mut router = admin_router(Arc::clone(runtime), move || metrics_handle.render());
    if let Some(extra) = extra_routes {
        router = router.merge(extra);
    }

    Some(serve(admin, router))
}
