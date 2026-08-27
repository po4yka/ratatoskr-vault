//! The typed configuration tree.
//!
//! One shape for the one deployable Vault has at this milestone. Worker roles may arrive later
//! (`docs/ARCHITECTURE.md` section 25); they will be added to this tree as new sections rather
//! than as parallel types, while no deployment exists that a rename would break.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use secrecy::{ExposeSecret as _, SecretString};
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

    /// Local mirror storage and the finite admission budgets for Git work.
    ///
    /// A process can serve its operator plane without this section, but it cannot run a mirror
    /// lifecycle until an operator deliberately supplies both confined roots and both budgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<MirrorLifecycleConfig>,

    /// Signed-manifest verification and finite isolated restore-drill policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationConfig>,

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
            mirror: None,
            verification: None,
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

/// The roots and byte budgets that make mirror work safe on the single deployment host.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorLifecycleConfig {
    /// `RATATOSKR__MIRROR__ROOT`: the absolute root whose `mirrors/` descendants Vault owns.
    pub root: PathBuf,

    /// `RATATOSKR__MIRROR__WORK_ROOT`: a separate absolute root for owned clone staging.
    pub work_root: PathBuf,

    /// `RATATOSKR__MIRROR__PER_MIRROR_MAX_BYTES`: positive ceiling for one mirror.
    pub per_mirror_max_bytes: u64,

    /// `RATATOSKR__MIRROR__GLOBAL_MAX_BYTES`: positive ceiling across all mirrors.
    pub global_max_bytes: u64,

    /// `RATATOSKR__MIRROR__MAX_CONCURRENT_OPERATIONS`: exactly four on the four-core host.
    #[serde(default = "default_max_concurrent_mirror_operations")]
    pub max_concurrent_operations: u8,
}

/// Current signed-manifest verification and restore-drill policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationConfig {
    /// Absolute Vault-owned root for UUID-derived restore scratch directories.
    pub scratch_root: PathBuf,
    /// Seconds between successful stored-artifact verifications.
    pub verification_frequency_seconds: u64,
    /// Seconds between successful isolated restore drills.
    pub drill_frequency_seconds: u64,
    /// Maximum snapshots admitted by one scheduler pass.
    pub sample_size: u32,
    /// Aggregate bundle bytes admitted to scratch by one scheduler pass.
    pub scratch_byte_budget: u64,
    /// Maximum concurrent drills on this four-core deployment target.
    pub max_concurrent: u8,
    /// Hard wall-clock deadline for every typed Git stage.
    pub per_drill_timeout_seconds: u64,
    /// SECRET: lowercase hex Ed25519 seed used only to sign new manifests.
    #[serde(default, skip_serializing)]
    pub manifest_signing_seed: SecretString,
    /// Trusted key-id to lowercase hex Ed25519 public-key mapping.
    pub trusted_manifest_public_keys: BTreeMap<String, String>,
}

impl VerificationConfig {
    /// Decodes the redacted signing seed after configuration validation.
    ///
    /// # Errors
    ///
    /// Returns a manifest-key error when called on an unvalidated configuration.
    pub fn manifest_signing_key(
        &self,
    ) -> Result<crate::snapshot::ManifestSigningKey, crate::snapshot::ManifestError> {
        let bytes = decode_key_hex(self.manifest_signing_seed.expose_secret())?;
        crate::snapshot::ManifestSigningKey::from_seed(bytes)
    }

    /// Decodes the explicit trusted public-key set after configuration validation.
    ///
    /// # Errors
    ///
    /// Returns a manifest-key error when called on an unvalidated configuration.
    pub fn trusted_manifest_keys(
        &self,
    ) -> Result<Vec<crate::snapshot::ManifestVerificationKey>, crate::snapshot::ManifestError> {
        self.trusted_manifest_public_keys
            .iter()
            .map(|(key_id, encoded)| {
                Ok(crate::snapshot::ManifestVerificationKey {
                    key_id: key_id.clone(),
                    public_key: decode_key_hex(encoded)?.to_vec(),
                })
            })
            .collect()
    }
}

fn decode_key_hex(encoded: &str) -> Result<[u8; 32], crate::snapshot::ManifestError> {
    if encoded.len() != 64 {
        return Err(crate::snapshot::ManifestError::InvalidVerificationKey);
    }
    let mut decoded = [0_u8; 32];
    for (destination, pair) in decoded.iter_mut().zip(encoded.as_bytes().chunks_exact(2)) {
        let [high_byte, low_byte] = pair else {
            return Err(crate::snapshot::ManifestError::InvalidVerificationKey);
        };
        let high = key_hex_value(*high_byte)
            .ok_or(crate::snapshot::ManifestError::InvalidVerificationKey)?;
        let low = key_hex_value(*low_byte)
            .ok_or(crate::snapshot::ManifestError::InvalidVerificationKey)?;
        *destination = (high << 4) | low;
    }
    Ok(decoded)
}

const fn key_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

const fn default_max_concurrent_mirror_operations() -> u8 {
    4
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
