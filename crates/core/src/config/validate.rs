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

    found
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
