//! The typed configuration tree.
//!
//! One shape for the one deployable Vault has at this milestone. Worker roles may arrive later
//! (`docs/ARCHITECTURE.md` section 25); they will be added to this tree as new sections rather
//! than as parallel types, while no deployment exists that a rename would break.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use secrecy::SecretString;
use url::Url;

/// Everything a Vault process must know before it can serve.
///
/// `Serialize` exists for exactly one reason — it seeds the built-in defaults provider. The one
/// secret member is `#[serde(skip_serializing)]`, so a default can never carry a secret and a
/// serialized configuration can never leak one.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// The operator listener: `/health/live`, `/health/ready`, `/metrics`, `/version`.
    pub admin: AdminConfig,

    /// The `PostgreSQL` connection holding the `git_vault` schema. Optional: a process without one
    /// starts, serves its probes, and reports no database check, because nothing else touches the
    /// database yet. The milestone that adds a route reading persisted data makes it required for
    /// starting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,

    /// The two phases of a graceful stop.
    pub shutdown: ShutdownConfig,

    /// Logging, filtering and span export.
    pub telemetry: TelemetryConfig,
}

impl VaultConfig {
    /// The built-in defaults. The ONLY place a default value is written, and the source of the
    /// table in `.env.example`.
    #[must_use]
    pub fn defaults() -> Self {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        Self {
            admin: AdminConfig {
                bind: SocketAddr::new(loopback, DEFAULT_ADMIN_PORT),
            },
            database: None,
            shutdown: ShutdownConfig {
                drain_seconds: default_drain_seconds(),
                grace_seconds: default_grace_seconds(),
            },
            telemetry: TelemetryConfig {
                log_format: LogFormat::default(),
                log_filter: default_log_filter(),
                otlp: None,
            },
        }
    }
}

/// The default operator-listener port.
///
/// Chosen once, against the fleet: platform holds `9464`-`9466` and every other service's ports
/// were measured before this number was written, so all four bounded contexts bind on one
/// developer machine without colliding.
pub const DEFAULT_ADMIN_PORT: u16 = 9570;

/// The operator plane. Never a public API surface: Vault serves no public listener at this
/// milestone, and when one arrives it binds separately.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// `RATATOSKR__ADMIN__BIND`. Default `127.0.0.1:9570`.
    ///
    /// Loopback by default because `SECURITY.md` says deny by default: an any-address default
    /// would silently publish `/metrics` on a developer's LAN. A deployment sets `0.0.0.0:<port>`
    /// deliberately, and startup warns about exactly that.
    pub bind: SocketAddr,
}

/// The `PostgreSQL` connection Vault owns. The `git_vault` schema only; no other schema is read or
/// written through this pool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// `RATATOSKR__DATABASE__URL`. The whole URL is a secret because a `PostgreSQL` URL carries the
    /// password in its user information, so it can never be `Debug`-printed.
    #[serde(default, skip_serializing)]
    pub url: SecretString,

    /// `RATATOSKR__DATABASE__MAX_CONNECTIONS`. 1..=100, default 10.
    ///
    /// A ceiling, not a target: `PostgreSQL`'s own `max_connections` is the real limit and the
    /// fleet's services share database hosts.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,

    /// `RATATOSKR__DATABASE__ACQUIRE_TIMEOUT_SECONDS`. 1..=30, default 5.
    ///
    /// Bounded low so a saturated pool surfaces as a fast, truthful failure rather than as work
    /// that hangs with no explanation.
    #[serde(default = "default_acquire_timeout_seconds")]
    pub acquire_timeout_seconds: u64,
}

const fn default_max_connections() -> u32 {
    10
}

const fn default_acquire_timeout_seconds() -> u64 {
    5
}

/// The two phases of a graceful stop. Separate knobs because they answer different questions: how
/// long until a supervisor notices, and how long an in-flight request may take.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownConfig {
    /// `RATATOSKR__SHUTDOWN__DRAIN_SECONDS`. 0..=60, default 5.
    ///
    /// Seconds to keep serving after SIGTERM while readiness already reports 503, so whatever
    /// routes to this process stops before the listener closes.
    #[serde(default = "default_drain_seconds")]
    pub drain_seconds: u64,

    /// `RATATOSKR__SHUTDOWN__GRACE_SECONDS`. 1..=120, default 25.
    /// Seconds allowed for in-flight requests after the listener stops accepting.
    #[serde(default = "default_grace_seconds")]
    pub grace_seconds: u64,
}

/// Logging and span export.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// `RATATOSKR__TELEMETRY__LOG_FORMAT`. Default `json`.
    #[serde(default)]
    pub log_format: LogFormat,

    /// `RATATOSKR__TELEMETRY__LOG_FILTER`. A `tracing_subscriber::EnvFilter` directive string.
    /// Default `info`. Validated at configuration time, so a bad filter is a configuration error
    /// on stderr rather than a failure inside telemetry initialisation.
    #[serde(default = "default_log_filter")]
    pub log_filter: String,

    /// `RATATOSKR__TELEMETRY__OTLP__*`. Absent means no span exporter.
    ///
    /// Absence does NOT mean absent trace ids: a provider with zero span processors still mints a
    /// valid, sampled, non-zero W3C trace id, so `trace_id` is real in every log line with no
    /// collector deployed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp: Option<OtlpConfig>,
}

/// How a log line is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// One JSON object per line. The default, because production log collectors parse it.
    #[default]
    Json,
    /// Human-readable, for `cargo run`.
    Pretty,
}

/// The OTLP span exporter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpConfig {
    /// `RATATOSKR__TELEMETRY__OTLP__ENDPOINT`, e.g. `https://collector.example:4317`.
    pub endpoint: Url,

    /// `RATATOSKR__TELEMETRY__OTLP__TIMEOUT_SECONDS`. 1..=60, default 5.
    #[serde(default = "default_otlp_timeout_seconds")]
    pub timeout_seconds: u64,

    /// `RATATOSKR__TELEMETRY__OTLP__HEADERS__<NAME>` — collector authentication.
    ///
    /// The only secret configured at this milestone. `Debug` renders `[REDACTED]`; there is no
    /// `Display`; `skip_serializing` means it cannot be written out; and the value is zeroized on
    /// drop.
    #[serde(default, skip_serializing)]
    pub headers: BTreeMap<String, SecretString>,
}

const fn default_drain_seconds() -> u64 {
    5
}

const fn default_grace_seconds() -> u64 {
    25
}

fn default_log_filter() -> String {
    "info".to_owned()
}

const fn default_otlp_timeout_seconds() -> u64 {
    5
}
