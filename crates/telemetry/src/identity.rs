//! The one wire identity of this bounded context, and the OpenTelemetry resource built from it.

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

/// The one wire identity of this bounded context, and the OpenTelemetry `service.name`.
///
/// Observability and wire identity read the same constant on purpose, so they cannot drift.
pub const SERVICE_NAME: &str = "ratatoskr-vault";

/// The crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Rust toolchain the binary was built with, from the workspace `rust-version`.
pub const RUST_VERSION: &str = env!("CARGO_PKG_RUST_VERSION");

/// The build's git commit, supplied by the container build as `RATATOSKR_GIT_SHA`; `"unknown"`
/// otherwise. Deliberately not a `build.rs` shelling out to `git`: a Docker build has no `.git`, so
/// that approach returns `"unknown"` exactly where the answer matters.
pub const GIT_SHA: &str = match option_env!("RATATOSKR_GIT_SHA") {
    Some(sha) => sha,
    None => "unknown",
};

/// The OpenTelemetry semantic-convention key `service.name`.
const SERVICE_NAME_KEY: &str = "service.name";

/// The OpenTelemetry semantic-convention key `service.namespace`.
const SERVICE_NAMESPACE_KEY: &str = "service.namespace";

/// The OpenTelemetry semantic-convention key `service.version`.
const SERVICE_VERSION_KEY: &str = "service.version";

/// The service namespace all Ratatoskr bounded contexts share.
const SERVICE_NAMESPACE: &str = "ratatoskr";

/// The OpenTelemetry resource for this process.
///
/// | attribute | value |
/// |---|---|
/// | `service.name` | [`SERVICE_NAME`] |
/// | `service.namespace` | `ratatoskr` |
/// | `service.version` | [`VERSION`] |
///
/// Dashboards facet on `service.name` within the shared namespace. There is deliberately no
/// `deployment.environment.name`: a scrape or collector configuration is the standard place for it
/// (Prometheus `external_labels`, the collector `resource` processor).
#[must_use]
pub fn resource() -> Resource {
    // `with_attributes` merges last-wins, so these override anything an environment detector may
    // have supplied.
    Resource::builder()
        .with_attributes([
            KeyValue::new(SERVICE_NAME_KEY, SERVICE_NAME),
            KeyValue::new(SERVICE_NAMESPACE_KEY, SERVICE_NAMESPACE),
            KeyValue::new(SERVICE_VERSION_KEY, VERSION),
        ])
        .build()
}
