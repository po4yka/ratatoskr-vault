//! Tracing subscriber, OpenTelemetry resource, wire identity, and metrics names.
//!
//! This crate deliberately has no axum and no sqlx: the persistence layer emits spans and the HTTP
//! harness renders metrics, and neither must inherit an HTTP server or a database pool to do it.

pub mod identity;
pub mod metrics;

use opentelemetry::trace::TracerProvider as _;
use ratatoskr_vault_core::config::{LogFormat, TelemetryConfig};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Installs the global tracing subscriber and the Prometheus recorder.
///
/// MUST be called from inside a Tokio runtime when `config.otlp` is set: the tonic exporter panics
/// with "there is no reactor running" when constructed outside one.
///
/// Layer order is load-bearing and was established by compilation in the sibling service:
///
/// ```text
/// Registry
///   .with(EnvFilter::new(&config.log_filter))
///   .with(tracing_opentelemetry::layer().with_tracer(tracer))   // BEFORE the format layer
///   .with(fmt::layer().json() | .pretty())
/// ```
///
/// # Errors
///
/// [`TelemetryError::Filter`] when `config.log_filter` is not a valid directive string — normally
/// unreachable, because configuration validation parses it first.
/// [`TelemetryError::Exporter`] when the OTLP span exporter or the Prometheus recorder cannot be
/// built. [`TelemetryError::AlreadyInstalled`] when this process already installed a subscriber or
/// a global metrics recorder.
pub fn init(config: &TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    let filter = EnvFilter::try_new(&config.log_filter).map_err(TelemetryError::Filter)?;
    let provider = tracer_provider(config)?;
    let metrics_handle = install_recorder()?;

    // The OpenTelemetry layer must be composed BEFORE the format layer; see the item documentation.
    let layers = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(identity::SERVICE_NAME)));

    let installed = match config.log_format {
        LogFormat::Json => layers
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_writer(std::io::stdout),
            )
            .try_init(),
        LogFormat::Pretty => layers
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_writer(std::io::stdout),
            )
            .try_init(),
    };
    installed.map_err(|_| TelemetryError::AlreadyInstalled)?;

    ::metrics::gauge!(
        metrics::VAULT_BUILD_INFO,
        "version" => identity::VERSION,
        "git_sha" => identity::GIT_SHA,
        "rust_version" => identity::RUST_VERSION,
    )
    .set(1.0);

    Ok(TelemetryGuard {
        provider,
        metrics_handle,
    })
}

/// Owns the tracer provider and the Prometheus handle for the lifetime of the process.
#[derive(Debug)]
pub struct TelemetryGuard {
    /// Kept alive so spans keep resolving; shut down explicitly, never from `Drop`.
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
    /// The Prometheus text-exposition renderer.
    metrics_handle: metrics_exporter_prometheus::PrometheusHandle,
}

impl TelemetryGuard {
    /// The Prometheus text-exposition renderer, for `GET /metrics`.
    #[must_use]
    pub fn metrics_handle(&self) -> metrics_exporter_prometheus::PrometheusHandle {
        self.metrics_handle.clone()
    }

    /// Flushes the span exporter and releases the providers. Idempotent.
    ///
    /// Called explicitly at the end of the shutdown sequence and NOT from `Drop`: a `Drop` that
    /// blocks on a network flush during a panic unwind is how a pod hangs.
    pub fn shutdown(self) {
        if let Err(error) = self.provider.shutdown() {
            tracing::warn!(%error, "the span exporter did not shut down cleanly");
        }
    }
}

/// Why telemetry could not be installed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TelemetryError {
    /// `telemetry.log_filter` is not a valid `EnvFilter` directive string.
    #[error("the log filter is not a valid tracing directive string")]
    Filter(#[source] tracing_subscriber::filter::ParseError),

    /// A span or metric exporter could not be constructed.
    ///
    /// The cause is interpolated because this error is reported at the one moment nothing else can
    /// report it. It is safe to interpolate because the source chain carries no header value, and
    /// configuration validation keeps a credential out of the endpoint URL.
    #[error("a telemetry exporter could not be constructed: {0}")]
    Exporter(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// This process already installed a global subscriber or a global metrics recorder.
    #[error("a global telemetry subscriber is already installed in this process")]
    AlreadyInstalled,
}

/// The tracer provider: with an OTLP batch exporter when one is configured, without one otherwise.
/// Both mint valid, sampled, non-zero W3C ids.
fn tracer_provider(
    config: &TelemetryConfig,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, TelemetryError> {
    let builder = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(identity::resource())
        .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOn);

    match config.otlp.as_ref() {
        Some(otlp) => Ok(builder.with_batch_exporter(span_exporter(otlp)?).build()),
        None => Ok(builder.build()),
    }
}

/// The OTLP/gRPC span exporter.
///
/// An `https` endpoint needs both halves of TLS: the crypto provider, which is a Cargo feature and
/// without which `build()` refuses the endpoint outright, and trust anchors.
fn span_exporter(
    otlp: &ratatoskr_vault_core::config::OtlpConfig,
) -> Result<opentelemetry_otlp::SpanExporter, TelemetryError> {
    use opentelemetry_otlp::WithExportConfig as _;
    use opentelemetry_otlp::WithTonicConfig as _;

    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(otlp.endpoint.as_str())
        .with_timeout(std::time::Duration::from_secs(otlp.timeout_seconds));

    if otlp.endpoint.scheme() == "https" {
        builder = builder.with_tls_config(
            opentelemetry_otlp::tonic_types::transport::ClientTlsConfig::new().with_enabled_roots(),
        );
    }

    if !otlp.headers.is_empty() {
        builder = builder.with_metadata(collector_metadata(otlp)?);
    }

    builder
        .build()
        .map_err(|error| TelemetryError::Exporter(Box::new(error)))
}

/// The collector authentication headers.
///
/// **The only site in the workspace that reads a secret in plaintext.** Every value is marked
/// sensitive, so `http` will not let a proxy log it.
fn collector_metadata(
    otlp: &ratatoskr_vault_core::config::OtlpConfig,
) -> Result<opentelemetry_otlp::tonic_types::metadata::MetadataMap, TelemetryError> {
    use secrecy::ExposeSecret as _;

    let mut headers = http::HeaderMap::with_capacity(otlp.headers.len());
    for (name, secret) in &otlp.headers {
        let name = http::HeaderName::try_from(name.as_str())
            .map_err(|error| TelemetryError::Exporter(Box::new(error)))?;
        let mut value = http::HeaderValue::from_str(secret.expose_secret())
            .map_err(|error| TelemetryError::Exporter(Box::new(error)))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(opentelemetry_otlp::tonic_types::metadata::MetadataMap::from_headers(headers))
}

/// Installs the global Prometheus recorder.
///
/// `default-features = false` drops the HTTP listener and the push gateway: `/metrics` is one axum
/// route on the admin router calling `handle.render()`, so there is no second HTTP server.
fn install_recorder() -> Result<metrics_exporter_prometheus::PrometheusHandle, TelemetryError> {
    use metrics_exporter_prometheus::{BuildError, PrometheusBuilder};

    PrometheusBuilder::new()
        .install_recorder()
        .map_err(|error| match error {
            BuildError::FailedToSetGlobalRecorder(_) => TelemetryError::AlreadyInstalled,
            other => TelemetryError::Exporter(Box::new(other)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatoskr_vault_core::config::OtlpConfig;

    /// A value that must never reach a log line, a `Debug` rendering or an error message.
    const CANARY: &str = "Bearer canary-not-a-real-credential";

    fn otlp_config() -> OtlpConfig {
        serde_json::from_value(serde_json::json!({
            "endpoint": "https://collector.example:4317",
            "timeout_seconds": 5,
            "headers": { "authorization": CANARY },
        }))
        .expect("the OTLP fixture must deserialize")
    }

    /// The one `expose_secret` call reaches the wire metadata, and nowhere else.
    #[test]
    fn collector_metadata_carries_the_header_and_never_renders_it() {
        let config = otlp_config();

        let metadata = collector_metadata(&config).expect("the metadata map must build");
        assert_eq!(
            metadata
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            Some(CANARY.to_owned()),
            "the header value must reach the wire"
        );

        assert!(
            !format!("{config:?}").contains("canary"),
            "a SecretString must render as [REDACTED] however deeply it is nested"
        );
    }

    /// An unusable header name or value fails as a build error that names neither the header nor
    /// its value. `TelemetryError::Exporter` interpolates its source, so this is what makes that
    /// interpolation safe.
    #[test]
    fn a_rejected_header_never_appears_in_the_error() {
        for (name, value) in [
            ("not a header name", CANARY.to_owned()),
            // A control character makes the VALUE unrepresentable, which is the other arm.
            ("authorization", format!("{CANARY}\n")),
        ] {
            let mut config = otlp_config();
            config.headers.insert(name.to_owned(), value.into());

            let error = collector_metadata(&config).expect_err("an invalid header must fail");
            let rendered = format!("{error:?} {error}");
            assert!(!rendered.contains("canary"), "got {rendered}");
        }
    }

    /// The `https` scheme validation accepts is a scheme the exporter can actually be built for.
    /// Without the `tls-*` features this fails and every process configured for a TLS-terminated
    /// collector exits 1 at startup.
    #[tokio::test]
    async fn the_span_exporter_builds_for_an_https_endpoint() {
        // The endpoint is connected to lazily, so this builds without a collector.
        span_exporter(&otlp_config()).expect("an https endpoint must be exportable");
    }

    /// The wire identity compiles from the manifest and cannot drift from it.
    #[test]
    fn the_wire_identity_names_this_bounded_context() {
        assert_eq!(identity::SERVICE_NAME, "ratatoskr-vault");
        assert_eq!(identity::VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(identity::RUST_VERSION, env!("CARGO_PKG_RUST_VERSION"));
        assert_eq!(
            identity::GIT_SHA,
            "unknown",
            "no RATATOSKR_GIT_SHA is set in tests"
        );
        assert!(
            !identity::resource()
                .iter()
                .any(|(key, _)| key.as_str() == "deployment.environment.name"),
            "environment belongs to the collector configuration, not the process"
        );
    }
}
