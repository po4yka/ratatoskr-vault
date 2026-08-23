//! The facts readiness is computed from, and the checks it reports.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ratatoskr_vault_telemetry::metrics::VAULT_READINESS;

/// No database is configured for this deployment.
const DATABASE_ABSENT: u8 = 0;
/// The last probe answered.
const DATABASE_UP: u8 = 1;
/// The last probe did not answer.
const DATABASE_DOWN: u8 = 2;

/// The facts readiness is computed from.
///
/// Shared by the admin router, which reads it, and the shutdown sequence, which writes it.
#[derive(Debug)]
pub struct RuntimeState {
    /// Configuration validated, telemetry installed, every configured listener bound.
    startup_complete: AtomicBool,
    /// A shutdown signal arrived.
    draining: AtomicBool,
    /// The database: 0 not configured, 1 answering, 2 not answering. Three states rather than a
    /// `bool`, because "no database" and "a database that is down" must not report the same thing.
    database: AtomicU8,
}

impl RuntimeState {
    /// A process that has bound nothing yet: readiness fails, liveness does not.
    #[must_use]
    pub fn new() -> Self {
        let state = Self {
            startup_complete: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            database: AtomicU8::new(DATABASE_ABSENT),
        };
        state.publish_readiness();
        state
    }

    /// Every listener is bound and telemetry is up. Set exactly once.
    pub fn mark_startup_complete(&self) {
        self.startup_complete.store(true, Ordering::Release);
        self.publish_readiness();
    }

    /// Record what the latest database probe found.
    ///
    /// Called by the prober, not by a request: a readiness probe must not open a connection, or a
    /// saturated pool would make the health check the thing that finishes it off.
    pub fn set_database_reachable(&self, reachable: bool) {
        self.database.store(
            if reachable {
                DATABASE_UP
            } else {
                DATABASE_DOWN
            },
            Ordering::Release,
        );
        self.publish_readiness();
    }

    /// What the last database probe found, or `None` when no database is configured.
    #[must_use]
    pub fn database_reachable(&self) -> Option<bool> {
        match self.database.load(Ordering::Acquire) {
            DATABASE_ABSENT => None,
            state => Some(state == DATABASE_UP),
        }
    }

    /// A shutdown signal arrived. Readiness fails immediately; the listeners stay open.
    pub fn begin_draining(&self) {
        self.draining.store(true, Ordering::Release);
        self.publish_readiness();
    }

    /// The readiness checks, sorted by name.
    ///
    /// A `Vec`, never a map, so two consecutive probe bodies are byte-identical and `diff` is a
    /// usable tool at 03:00. There is deliberately no registry and no trait: a trait with one
    /// implementation is the abstraction this project rejects.
    #[must_use]
    pub fn checks(&self) -> Vec<Check> {
        let draining = self.draining.load(Ordering::Acquire);
        let started = self.startup_complete.load(Ordering::Acquire);
        let mut checks = vec![
            Check {
                name: CheckName::Drain,
                state: CheckState::from_pass(!draining),
                reason: draining.then_some(CheckReason::ShutdownRequested),
            },
            Check {
                name: CheckName::Startup,
                state: CheckState::from_pass(started),
                reason: (!started).then_some(CheckReason::StartupIncomplete),
            },
        ];

        // An unconfigured database reports NO check at all, not a passing one: a passing check
        // for something that does not exist is the readiness equivalent of an always-zero metric.
        // The declaration order above is alphabetical by variant, so inserting at the front keeps
        // the body stable without a sort.
        match self.database_reachable() {
            None => {}
            Some(up) => checks.insert(
                0,
                Check {
                    name: CheckName::Database,
                    state: CheckState::from_pass(up),
                    reason: (!up).then_some(CheckReason::DependencyUnavailable),
                },
            ),
        }

        checks
    }

    /// Whether new work may be routed to this process.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.startup_complete.load(Ordering::Acquire) && !self.draining.load(Ordering::Acquire)
    }

    /// `vault_readiness`, the aggregate of [`Self::checks`].
    fn publish_readiness(&self) {
        let value = if self.is_ready() { 1.0 } else { 0.0 };
        metrics::gauge!(VAULT_READINESS).set(value);
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

/// One readiness check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Check {
    /// The logical name of the subject.
    pub name: CheckName,
    /// Whether the subject passes.
    pub state: CheckState,
    /// Why it does not, when it does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<CheckReason>,
}

/// A logical token from a closed set. Never a hostname, port, DSN or driver message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckName {
    /// The database answers. Present only when one is configured.
    Database,
    /// No shutdown signal has arrived.
    Drain,
    /// Configuration, telemetry and every configured listener are up.
    Startup,
}

/// Whether one check passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// The subject is healthy.
    Pass,
    /// The subject is not healthy.
    Fail,
}

impl CheckState {
    /// The state a boolean subject is in.
    fn from_pass(pass: bool) -> Self {
        if pass { Self::Pass } else { Self::Fail }
    }
}

/// A closed set. NEVER a formatted dependency error: a driver message can carry a host, a port,
/// a user name and sometimes a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckReason {
    /// The process has not finished binding its listeners.
    StartupIncomplete,
    /// A shutdown signal arrived and this instance is draining.
    ShutdownRequested,
    /// The last probe of the database did not answer.
    DependencyUnavailable,
}
