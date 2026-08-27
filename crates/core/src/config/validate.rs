//! The startup validation rules and the operator-facing failure report.
//!
//! Order at startup is strictly: extract, validate, initialise telemetry, bind listeners. Telemetry
//! is initialised *after* validation so that an invalid `log_filter` fails as a configuration
//! problem on stderr rather than inside subscriber setup, where nothing could report it.
//!
//! figment's extraction is fail-fast, so the "report every problem" guarantee comes from this pass
//! and not from serde.

use std::fmt::Write as _;

use tracing_subscriber::EnvFilter;

use crate::config::model::VaultConfig;
use secrecy::ExposeSecret as _;

/// One startup-rule violation.
///
/// Every member is `&'static str`. It is therefore STRUCTURALLY IMPOSSIBLE for a supplied value to
/// appear in a configuration failure report, so the report can never echo a secret. This is a type
/// property, not a rule someone has to remember.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The dotted configuration path, e.g. `database.url`.
    pub key: &'static str,
    /// The environment variable that sets it, e.g. `RATATOSKR__DATABASE__URL`.
    pub env_var: &'static str,
    /// What the rule requires, and the document that requires it.
    pub rule: &'static str,
}

/// The ceiling on `drain_seconds + grace_seconds`.
///
/// A supervisor's own stop timeout must EXCEED it: systemd's default `TimeoutStopSec` is 90
/// seconds, so a configuration this rule accepts would be `SIGKILL`ed thirty seconds into a drain
/// it was told to perform.
pub const SHUTDOWN_CEILING_SECONDS: u64 = 120;

/// Applies every startup rule and returns every violation found, in rule order.
pub(crate) fn validate(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();

    // V1 — a bad filter otherwise silences every log line at the moment you need them.
    if EnvFilter::try_new(&config.telemetry.log_filter).is_err() {
        found.push(Violation {
            key: "telemetry.log_filter",
            env_var: "RATATOSKR__TELEMETRY__LOG_FILTER",
            rule: "must parse as a tracing-subscriber EnvFilter directive string, e.g. info,vault=debug",
        });
    }

    // V2 — a total above the supervisor's stop timeout guarantees SIGKILL mid-request.
    let drain = config.shutdown.drain_seconds;
    let grace = config.shutdown.grace_seconds;
    if drain > 60 {
        found.push(Violation {
            key: "shutdown.drain_seconds",
            env_var: "RATATOSKR__SHUTDOWN__DRAIN_SECONDS",
            rule: "must be 0..=60, and drain_seconds + grace_seconds must not exceed 120",
        });
    }
    if !(1..=SHUTDOWN_CEILING_SECONDS).contains(&grace)
        || drain.saturating_add(grace) > SHUTDOWN_CEILING_SECONDS
    {
        found.push(Violation {
            key: "shutdown.grace_seconds",
            env_var: "RATATOSKR__SHUTDOWN__GRACE_SECONDS",
            rule: "must be 1..=120, and drain_seconds + grace_seconds must not exceed 120",
        });
    }

    found.extend(database_violations(config));
    found.extend(otlp_violations(config));
    found.extend(mirror_violations(config));
    found.extend(verification_violations(config));
    found.extend(replica_violations(config));

    found
}

/// V7 — off-host work is permitted only with explicit credentials, transport confinement, and
/// finite admission/deadline budgets. Values never enter a violation, so secrets remain redacted.
fn replica_violations(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(replicas) = config.replicas.as_ref() else {
        return found;
    };

    for target in replicas.targets.values().filter(|target| target.enabled) {
        let loopback_http = target.endpoint.scheme() == "http"
            && target.endpoint.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
        let endpoint_is_origin = target.endpoint.username().is_empty()
            && target.endpoint.password().is_none()
            && target.endpoint.query().is_none()
            && target.endpoint.fragment().is_none()
            && target.endpoint.path() == "/";
        if (target.endpoint.scheme() != "https" && !loopback_http) || !endpoint_is_origin {
            found.push(Violation {
                key: "replicas.targets.<target>.endpoint",
                env_var: "RATATOSKR__REPLICAS__TARGETS__<TARGET>__ENDPOINT",
                rule: "must be a credential-free https:// origin, except loopback http:// origins used by local tests",
            });
        }
        if target.access_key.expose_secret().is_empty() {
            found.push(Violation {
                key: "replicas.targets.<target>.access_key",
                env_var: "RATATOSKR__REPLICAS__TARGETS__<TARGET>__ACCESS_KEY",
                rule: "must be supplied through the process environment",
            });
        }
        if target.secret_access_key.expose_secret().is_empty() {
            found.push(Violation {
                key: "replicas.targets.<target>.secret_access_key",
                env_var: "RATATOSKR__REPLICAS__TARGETS__<TARGET>__SECRET_ACCESS_KEY",
                rule: "must be supplied through the process environment",
            });
        }
        replica_positive(
            &mut found,
            target.connect_timeout_seconds,
            "replicas.targets.<target>.connect_timeout_seconds",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__CONNECT_TIMEOUT_SECONDS",
            "must be a positive finite connection timeout",
        );
        replica_positive(
            &mut found,
            target.request_timeout_seconds,
            "replicas.targets.<target>.request_timeout_seconds",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__REQUEST_TIMEOUT_SECONDS",
            "must be a positive finite end-to-end deadline",
        );
        replica_positive(
            &mut found,
            target.attempt_timeout_seconds,
            "replicas.targets.<target>.attempt_timeout_seconds",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__ATTEMPT_TIMEOUT_SECONDS",
            "must be a positive finite attempt timeout",
        );
        replica_positive(
            &mut found,
            target.max_object_bytes,
            "replicas.targets.<target>.max_object_bytes",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__MAX_OBJECT_BYTES",
            "must be a positive finite object-size ceiling",
        );
        replica_positive(
            &mut found,
            u64::from(target.max_backlog_items),
            "replicas.targets.<target>.max_backlog_items",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__MAX_BACKLOG_ITEMS",
            "must be a positive finite item ceiling",
        );
        replica_positive(
            &mut found,
            target.max_backlog_bytes,
            "replicas.targets.<target>.max_backlog_bytes",
            "RATATOSKR__REPLICAS__TARGETS__<TARGET>__MAX_BACKLOG_BYTES",
            "must be a positive finite byte ceiling",
        );
        if !(1..=4).contains(&target.max_concurrent) {
            found.push(Violation {
                key: "replicas.targets.<target>.max_concurrent",
                env_var: "RATATOSKR__REPLICAS__TARGETS__<TARGET>__MAX_CONCURRENT",
                rule: "must be 1..=4 for the deployment target's four CPU cores",
            });
        }
    }

    found
}

fn replica_positive(
    found: &mut Vec<Violation>,
    value: u64,
    key: &'static str,
    env_var: &'static str,
    rule: &'static str,
) {
    if value == 0 {
        found.push(Violation { key, env_var, rule });
    }
}

/// V6 — verification never runs without finite admission, confinement, and explicit trust roots.
fn verification_violations(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(verification) = config.verification.as_ref() else {
        return found;
    };

    let scratch_overlaps_live_storage = config.mirror.as_ref().is_none_or(|mirror| {
        paths_overlap(&verification.scratch_root, &mirror.root)
            || paths_overlap(&verification.scratch_root, &mirror.work_root)
    });
    if !verification.scratch_root.is_absolute() || scratch_overlaps_live_storage {
        found.push(Violation {
            key: "verification.scratch_root",
            env_var: "RATATOSKR__VERIFICATION__SCRATCH_ROOT",
            rule: "must be an absolute root disjoint from configured mirror and work roots",
        });
    }
    if verification.verification_frequency_seconds == 0 {
        found.push(Violation {
            key: "verification.verification_frequency_seconds",
            env_var: "RATATOSKR__VERIFICATION__VERIFICATION_FREQUENCY_SECONDS",
            rule: "must be a positive finite interval",
        });
    }
    if verification.drill_frequency_seconds == 0 {
        found.push(Violation {
            key: "verification.drill_frequency_seconds",
            env_var: "RATATOSKR__VERIFICATION__DRILL_FREQUENCY_SECONDS",
            rule: "must be a positive finite interval",
        });
    }
    if verification.sample_size == 0 {
        found.push(Violation {
            key: "verification.sample_size",
            env_var: "RATATOSKR__VERIFICATION__SAMPLE_SIZE",
            rule: "must be a positive finite sample",
        });
    }
    if verification.scratch_byte_budget == 0 {
        found.push(Violation {
            key: "verification.scratch_byte_budget",
            env_var: "RATATOSKR__VERIFICATION__SCRATCH_BYTE_BUDGET",
            rule: "must be a positive finite byte budget",
        });
    }
    if !(1..=4).contains(&verification.max_concurrent) {
        found.push(Violation {
            key: "verification.max_concurrent",
            env_var: "RATATOSKR__VERIFICATION__MAX_CONCURRENT",
            rule: "must be 1..=4 for the deployment target's four CPU cores",
        });
    }
    if verification.per_drill_timeout_seconds == 0 {
        found.push(Violation {
            key: "verification.per_drill_timeout_seconds",
            env_var: "RATATOSKR__VERIFICATION__PER_DRILL_TIMEOUT_SECONDS",
            rule: "must be a positive finite process deadline",
        });
    }
    if !is_lower_hex(verification.manifest_signing_seed.expose_secret(), 64) {
        found.push(Violation {
            key: "verification.manifest_signing_seed",
            env_var: "RATATOSKR__VERIFICATION__MANIFEST_SIGNING_SEED",
            rule: "must be a 32-byte lowercase hexadecimal Ed25519 seed",
        });
    }
    if verification.trusted_manifest_public_keys.is_empty()
        || verification
            .trusted_manifest_public_keys
            .iter()
            .any(|(key_id, public_key)| !is_lower_hex(key_id, 64) || !is_lower_hex(public_key, 64))
    {
        found.push(Violation {
            key: "verification.trusted_manifest_public_keys",
            env_var: "RATATOSKR__VERIFICATION__TRUSTED_MANIFEST_PUBLIC_KEYS__<KEY_ID>",
            rule: "must contain lowercase SHA-256 key ids mapped to 32-byte lowercase hexadecimal Ed25519 public keys",
        });
    }

    found
}

fn paths_overlap(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn is_lower_hex(value: &str, expected_length: usize) -> bool {
    value.len() == expected_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// V5 — a configured lifecycle has finite positive storage budgets and exactly the host's four
/// execution permits. There is no "unlimited" safety fallback: absence disables lifecycle work.
fn mirror_violations(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(mirror) = config.mirror.as_ref() else {
        return found;
    };

    if !mirror.root.is_absolute() {
        found.push(Violation {
            key: "mirror.root",
            env_var: "RATATOSKR__MIRROR__ROOT",
            rule: "must be an absolute confined storage root",
        });
    }
    if !mirror.work_root.is_absolute() || mirror.work_root == mirror.root {
        found.push(Violation {
            key: "mirror.work_root",
            env_var: "RATATOSKR__MIRROR__WORK_ROOT",
            rule: "must be an absolute staging root distinct from mirror.root",
        });
    }
    if mirror.per_mirror_max_bytes == 0 {
        found.push(Violation {
            key: "mirror.per_mirror_max_bytes",
            env_var: "RATATOSKR__MIRROR__PER_MIRROR_MAX_BYTES",
            rule: "must be a positive finite byte budget",
        });
    }
    if mirror.global_max_bytes == 0 {
        found.push(Violation {
            key: "mirror.global_max_bytes",
            env_var: "RATATOSKR__MIRROR__GLOBAL_MAX_BYTES",
            rule: "must be a positive finite byte budget",
        });
    }
    if mirror.max_concurrent_operations != 4 {
        found.push(Violation {
            key: "mirror.max_concurrent_operations",
            env_var: "RATATOSKR__MIRROR__MAX_CONCURRENT_OPERATIONS",
            rule: "must equal 4, the deployment target's four CPU cores",
        });
    }

    found
}

/// V3 — the database rules. Absent means no rules: an absent database is a deployment shape this
/// milestone supports, not a violation.
fn database_violations(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(database) = config.database.as_ref() else {
        return found;
    };

    if !(1..=100).contains(&database.max_connections) {
        found.push(Violation {
            key: "database.max_connections",
            env_var: "RATATOSKR__DATABASE__MAX_CONNECTIONS",
            rule: "must be 1..=100",
        });
    }

    if !(1..=30).contains(&database.acquire_timeout_seconds) {
        found.push(Violation {
            key: "database.acquire_timeout_seconds",
            env_var: "RATATOSKR__DATABASE__ACQUIRE_TIMEOUT_SECONDS",
            rule: "must be 1..=30",
        });
    }

    // The scheme. `postgres://` and `postgresql://` are the two sqlx accepts; anything else fails
    // at connect time, which is after the process has already reported itself started. A
    // configuration error must be a startup error.
    let url = database.url.expose_secret();
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        found.push(Violation {
            key: "database.url",
            env_var: "RATATOSKR__DATABASE__URL",
            rule: "must be a postgres:// or postgresql:// URL",
        });
    }

    found
}

/// V4 — the OTLP rules, checked here rather than at exporter construction for the same reason the
/// filter is checked here: the failure must name a variable on stderr while there is still time to
/// fix it.
fn otlp_violations(config: &VaultConfig) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(otlp) = config.telemetry.otlp.as_ref() else {
        return found;
    };

    // An `https` endpoint needs a TLS feature the exporter enables; a plain-HTTP collector is
    // legal but unusual enough to be a deliberate choice. Anything else cannot be dialed at all.
    if !matches!(otlp.endpoint.scheme(), "http" | "https") {
        found.push(Violation {
            key: "telemetry.otlp.endpoint",
            env_var: "RATATOSKR__TELEMETRY__OTLP__ENDPOINT",
            rule: "must be an http:// or https:// URL of an OTLP/gRPC collector",
        });
    }

    if !(1..=60).contains(&otlp.timeout_seconds) {
        found.push(Violation {
            key: "telemetry.otlp.timeout_seconds",
            env_var: "RATATOSKR__TELEMETRY__OTLP__TIMEOUT_SECONDS",
            rule: "must be 1..=60",
        });
    }

    for name in otlp.headers.keys() {
        if !is_header_name(name) {
            found.push(Violation {
                key: "telemetry.otlp.headers.<name>",
                env_var: "RATATOSKR__TELEMETRY__OTLP__HEADERS__<NAME>",
                rule: "each header name must be 1..=64 lowercase letters, digits or hyphens",
            });
        }
    }

    found
}

/// `^[a-z0-9-]{1,64}$`, spelled without a regular-expression dependency.
fn is_header_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The operator-facing report for a set of violations. One block per problem, stable order, no
/// supplied values.
pub(crate) fn report_invalid(violations: &[Violation]) -> String {
    let plural = if violations.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "ratatoskr-vault: refusing to start; {} configuration problem{plural}.\n\n",
        violations.len(),
    );
    for violation in violations {
        let _ = writeln!(
            out,
            "  {}\n      {}\n      {}\n",
            violation.key, violation.env_var, violation.rule
        );
    }
    push_footer(&mut out);
    out
}

/// The operator-facing report for an extraction failure.
///
/// figment's message is deliberately NOT interpolated: it can quote the supplied value, and a
/// configuration report that echoes a value can echo a secret. Only keys are named.
pub(crate) fn report_unreadable(error: &figment::Error) -> String {
    let mut out =
        "ratatoskr-vault: refusing to start; the configuration could not be read.\n\n".to_owned();
    for problem in error.clone() {
        let key = key_of(&problem);
        let _ = writeln!(
            out,
            "  {key}\n      {}\n      {}\n",
            env_var_of(&key),
            reason_of(&problem),
        );
    }
    push_footer(&mut out);
    out
}

/// The two closing lines every report ends with.
fn push_footer(out: &mut String) {
    let _ = write!(
        out,
        "Supplied values are never echoed.\nValidate without starting: ratatoskr-vault check-config\n"
    );
}

/// The dotted key an extraction failure is about; keys are safe to print, values are not.
fn key_of(error: &figment::Error) -> String {
    use figment::error::Kind;
    let path = error.path.join(".");
    match &error.kind {
        // figment reports a missing member under its PARENT's path, so appending the member's own
        // name is what makes the block actionable:
        // `telemetry.otlp.endpoint`, not `telemetry.otlp`.
        Kind::MissingField(name) if path.is_empty() => name.to_string(),
        Kind::MissingField(name) => format!("{path}.{name}"),
        _ if !path.is_empty() => path,
        Kind::UnknownField(name, _) => name.clone(),
        _ => "(the provider did not report a key)".to_owned(),
    }
}

/// The environment variable a dotted key is set by.
fn env_var_of(key: &str) -> String {
    format!("RATATOSKR__{}", key.replace('.', "__").to_uppercase())
}

/// What went wrong, in terms that never quote the supplied value.
const fn reason_of(error: &figment::Error) -> &'static str {
    match &error.kind {
        figment::error::Kind::UnknownField(_, _) => "is not a configuration key of this process",
        figment::error::Kind::MissingField(_) => "is required and was not supplied",
        _ => "could not be read as the type of this field",
    }
}
