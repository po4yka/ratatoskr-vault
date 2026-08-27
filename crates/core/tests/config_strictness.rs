//! Configuration strictness: the behaviors an operator relies on when a deployment refuses to
//! start. Spec: `configuration`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "a test builds figment providers directly instead of reading the process environment"
)]
#![allow(
    clippy::result_large_err,
    reason = "the helper returns the loader's own Result; boxing would only move the lint"
)]

use figment::providers::Serialized;
use ratatoskr_vault_core::config::{self, ConfigError, VaultConfig};
use secrecy::ExposeSecret as _;

const CANARY_PASSWORD: &str = "canary-not-a-real-password";

/// A figment over the defaults with `vars` layered on top. Keys are the dotted paths that
/// `Env::prefixed("RATATOSKR__").split("__")` produces from the documented variable names:
/// `RATATOSKR__ADMIN__BIND` arrives here as `admin.bind`.
fn config_from(vars: &[(&str, String)]) -> Result<VaultConfig, ConfigError> {
    let mut figment = config::figment();
    for (key, value) in vars {
        figment = figment.merge(Serialized::default(key, value.clone()));
    }
    config::load_from(figment)
}

/// A valid database URL carrying a secret, for the redaction tests.
fn secret_database_url() -> String {
    format!("postgres://vault:{CANARY_PASSWORD}@127.0.0.1:5432/vault")
}

/// The environment naming scheme is an operational contract: a nested variable populates the
/// nested field.
#[test]
fn a_nested_variable_populates_the_nested_field() {
    let loaded = config_from(&[("admin.bind", "127.0.0.1:9501".to_owned())])
        .expect("a documented variable must load");

    assert_eq!(loaded.admin.bind.to_string(), "127.0.0.1:9501");
}

/// Unknown keys are refused, never ignored: a typo in a deployment must not silently configure
/// nothing.
#[test]
fn an_unknown_key_is_refused() {
    let error = config_from(&[("no_such_section.x", "1".to_owned())])
        .expect_err("an unknown key must fail extraction");

    let report = error.report();
    assert!(
        report.contains("NO_SUCH_SECTION"),
        "the report must name the unknown key\n{report}"
    );
}

/// A value that does not parse into its field type fails extraction naming the key.
#[test]
fn a_wrong_type_is_refused_naming_the_key() {
    let error = config_from(&[("admin.bind", "not-a-socket-address".to_owned())])
        .expect_err("an unparseable bind must fail extraction");

    let report = error.report();
    assert!(
        report.contains("admin.bind"),
        "the report must name the key\n{report}"
    );
    assert!(
        !report.contains("not-a-socket-address"),
        "the report must not echo the supplied value\n{report}"
    );
}

/// V1: a bad filter otherwise silences every log line exactly when they are needed.
#[test]
fn an_invalid_log_filter_is_a_configuration_error() {
    let error = config_from(&[("telemetry.log_filter", "=not-a-directive".to_owned())])
        .expect_err("an invalid filter must fail validation");

    let report = error.report();
    assert!(
        report.contains("telemetry.log_filter"),
        "the report must name the key\n{report}"
    );
}

/// V2: the drain-plus-grace total is bounded by the supervisor's stop timeout.
#[test]
fn a_shutdown_total_above_the_ceiling_is_refused() {
    // Typed values, as the environment provider would parse the numbers out of their strings.
    let error = config::load_from(
        config::figment()
            .merge(Serialized::default("shutdown.drain_seconds", 60_u64))
            .merge(Serialized::default("shutdown.grace_seconds", 120_u64)),
    )
    .expect_err("drain 60 + grace 120 exceeds the 120-second ceiling");

    let report = error.report();
    assert!(
        report.contains("shutdown.grace_seconds"),
        "the report must name the offending key\n{report}"
    );
}

/// V3: a URL that is not a `PostgreSQL` URL would only fail at connect time, after the process
/// reported itself started. A configuration error must be a startup error.
#[test]
fn a_non_postgres_database_url_is_refused() {
    let error = config_from(&[("database.url", "mysql://127.0.0.1:3306/vault".to_owned())])
        .expect_err("a non-postgres URL must fail validation");

    let report = error.report();
    assert!(
        report.contains("database.url"),
        "the report must name the key\n{report}"
    );
}

/// Validation collects every violation into one round trip: an operator editing an environment
/// wants the whole list, not the first problem five times.
#[test]
fn every_violation_is_reported_in_one_round_trip() {
    // Typed values, as the environment provider would parse the numbers out of their strings.
    // A valid URL is supplied so that materializing the database section is not itself the
    // violation under test.
    let error = config::load_from(
        config::figment()
            .merge(Serialized::default(
                "telemetry.log_filter",
                "=not-a-directive",
            ))
            .merge(Serialized::default("shutdown.drain_seconds", 61_u64))
            .merge(Serialized::default(
                "database.url",
                "postgres://vault@127.0.0.1:5432/vault",
            ))
            .merge(Serialized::default("database.max_connections", 0_u32)),
    )
    .expect_err("three independent rules are violated at once");

    let ConfigError::Invalid(violations) = &error else {
        panic!("expected Invalid, got {error:?}");
    };
    assert_eq!(violations.len(), 3, "all three violations must be reported");

    let report = error.report();
    for key in [
        "telemetry.log_filter",
        "shutdown.drain_seconds",
        "database.max_connections",
    ] {
        assert!(report.contains(key), "the report must name {key}\n{report}");
    }
}

/// A secret configured by environment reaches neither the failure report nor any debug rendering.
#[test]
fn a_database_credential_never_reaches_a_report_or_a_debug_rendering() {
    // The URL is valid; the drain violation is what makes this path fail while carrying the
    // secret through extraction.
    let error = config_from(&[
        ("database.url", secret_database_url()),
        ("shutdown.drain_seconds", "61".to_owned()),
    ])
    .expect_err("the drain violation must fire");
    let rendered = format!("{} {error:?}", error.report());
    assert!(
        !rendered.contains(CANARY_PASSWORD),
        "the credential leaked into the failure path: {rendered}"
    );

    // And the valid path: the loaded configuration itself must not render it either.
    let loaded = config_from(&[("database.url", secret_database_url())])
        .expect("a well-formed database URL must load");
    assert!(
        !format!("{loaded:?}").contains(CANARY_PASSWORD),
        "SecretString must render as [REDACTED] however deeply nested"
    );
    let database = loaded.database.expect("the database section exists");
    assert_eq!(
        database.url.expose_secret(),
        &secret_database_url(),
        "the secret must still be readable through the one sanctioned accessor"
    );
}

/// V4: an endpoint the exporter cannot dial is a configuration error, not a startup crash.
#[test]
fn a_non_http_otlp_endpoint_is_refused() {
    let error = config_from(&[(
        "telemetry.otlp.endpoint",
        "grpc://collector.example:4317".to_owned(),
    )])
    .expect_err("a grpc:// endpoint must fail validation");

    let report = error.report();
    assert!(
        report.contains("telemetry.otlp.endpoint"),
        "the report must name the key\n{report}"
    );
}

/// An absent database is a supported deployment shape, not a violation: the process starts and
/// reports no database check.
#[test]
fn an_absent_database_section_loads_with_no_database() {
    let loaded = config_from(&[]).expect("the defaults alone must load");

    assert!(loaded.database.is_none());
    assert_eq!(loaded.admin.bind.to_string(), "127.0.0.1:9570");
    assert_eq!(loaded.telemetry.log_filter, "info");
}

/// Mirror work is enabled only with finite positive budgets, and the deployment target admits no
/// more than four simultaneous Git operations on its four CPU cores.
#[test]
fn mirror_lifecycle_budget_requires_finite_positive_limits() {
    let error = config::load_from(
        config::figment()
            .merge(Serialized::default(
                "mirror.root",
                "/var/lib/ratatoskr/mirrors".to_owned(),
            ))
            .merge(Serialized::default(
                "mirror.work_root",
                "/var/lib/ratatoskr/work".to_owned(),
            ))
            .merge(Serialized::default("mirror.per_mirror_max_bytes", 0_u64))
            .merge(Serialized::default("mirror.global_max_bytes", 0_u64))
            .merge(Serialized::default(
                "mirror.max_concurrent_operations",
                5_u8,
            )),
    )
    .expect_err("zero budgets and a fifth operation must be rejected");

    let report = error.report();
    for key in [
        "mirror.per_mirror_max_bytes",
        "mirror.global_max_bytes",
        "mirror.max_concurrent_operations",
    ] {
        assert!(report.contains(key), "the report must name {key}\n{report}");
    }
}

#[test]
fn verification_policy_rejects_zero_budgets_overlapping_roots_and_bad_keys() {
    use std::collections::BTreeMap;

    let signing_seed_canary = "not-a-valid-secret-signing-seed";
    let error = config::load_from(
        config::figment()
            .merge(Serialized::default(
                "mirror.root",
                "/var/lib/ratatoskr/mirrors",
            ))
            .merge(Serialized::default(
                "mirror.work_root",
                "/var/lib/ratatoskr/work",
            ))
            .merge(Serialized::default("mirror.per_mirror_max_bytes", 1024_u64))
            .merge(Serialized::default("mirror.global_max_bytes", 4096_u64))
            .merge(Serialized::default(
                "mirror.max_concurrent_operations",
                4_u8,
            ))
            .merge(Serialized::default(
                "verification.scratch_root",
                "/var/lib/ratatoskr/mirrors/scratch",
            ))
            .merge(Serialized::default(
                "verification.verification_frequency_seconds",
                0_u64,
            ))
            .merge(Serialized::default(
                "verification.drill_frequency_seconds",
                0_u64,
            ))
            .merge(Serialized::default("verification.sample_size", 0_u32))
            .merge(Serialized::default(
                "verification.scratch_byte_budget",
                0_u64,
            ))
            .merge(Serialized::default("verification.max_concurrent", 0_u8))
            .merge(Serialized::default(
                "verification.per_drill_timeout_seconds",
                0_u64,
            ))
            .merge(Serialized::default(
                "verification.manifest_signing_seed",
                signing_seed_canary,
            ))
            .merge(Serialized::default(
                "verification.trusted_manifest_public_keys",
                BTreeMap::from([("bad-key", "bad-public-key")]),
            )),
    )
    .expect_err("unsafe verification policy and key material must be rejected");

    let rendered = format!("{} {error:?}", error.report());
    for key in [
        "verification.scratch_root",
        "verification.verification_frequency_seconds",
        "verification.drill_frequency_seconds",
        "verification.sample_size",
        "verification.scratch_byte_budget",
        "verification.max_concurrent",
        "verification.per_drill_timeout_seconds",
        "verification.manifest_signing_seed",
        "verification.trusted_manifest_public_keys",
    ] {
        assert!(
            rendered.contains(key),
            "the report must name {key}\n{rendered}"
        );
    }
    assert!(
        !rendered.contains(signing_seed_canary),
        "the secret signing seed leaked into diagnostics: {rendered}"
    );
}

#[test]
fn replica_policy_rejects_missing_secrets_zero_budgets_and_plaintext_remote() {
    let secret_canary = "replica-secret-must-never-render";
    let error = config::load_from(config::figment().merge(Serialized::default(
        "replicas",
        serde_json::json!({
            "targets": {
                "offsite": {
                    "endpoint": "http://storage.example.invalid",
                    "bucket": "vault-fixtures",
                    "region": "fixture-1",
                    "secret_access_key": secret_canary,
                    "connect_timeout_seconds": 0,
                    "request_timeout_seconds": 0,
                    "attempt_timeout_seconds": 0,
                    "max_object_bytes": 0,
                    "max_backlog_items": 0,
                    "max_backlog_bytes": 0,
                    "max_concurrent": 0
                }
            }
        }),
    )))
    .expect_err("missing access key, plaintext remote endpoint, and zero limits must be rejected");

    let rendered = format!("{} {error:?}", error.report());
    for key in [
        "replicas.targets.<target>.endpoint",
        "replicas.targets.<target>.access_key",
        "replicas.targets.<target>.connect_timeout_seconds",
        "replicas.targets.<target>.request_timeout_seconds",
        "replicas.targets.<target>.attempt_timeout_seconds",
        "replicas.targets.<target>.max_object_bytes",
        "replicas.targets.<target>.max_backlog_items",
        "replicas.targets.<target>.max_backlog_bytes",
        "replicas.targets.<target>.max_concurrent",
    ] {
        assert!(
            rendered.contains(key),
            "the report must name {key}\n{rendered}"
        );
    }
    assert!(
        !rendered.contains(secret_canary),
        "replica credentials leaked into diagnostics: {rendered}"
    );
}
