//! The admin router: liveness, readiness, metrics and version, on the operator listener only.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use http::{HeaderValue, StatusCode, header};
use ratatoskr_vault_telemetry::identity;

use crate::lifecycle::{Check, RuntimeState};

/// The Prometheus text exposition format the `metrics` crate renders.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// The admin plane carries no client error envelope.
///
/// `/health/ready` returning 503 must tell an operator WHICH check failed. These bodies are read
/// by a person and by a metrics scrape, not by a Vault client. An unknown admin path returns a
/// bare 404.
///
/// All four routes respond `Cache-Control: no-store`: a cached "ready" is a routing decision made
/// from stale data.
///
/// `render_metrics` is a closure rather than a `PrometheusHandle` so that this crate does not
/// depend on the exporter — `ratatoskr-vault-telemetry` owns that dependency, and the admin plane
/// only needs the text.
pub fn admin_router<R>(state: Arc<RuntimeState>, render_metrics: R) -> Router
where
    R: Fn() -> String + Send + Sync + 'static,
{
    let state = Arc::new(AdminState {
        runtime: state,
        render_metrics: Box::new(render_metrics),
    });
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/version", get(version))
        .with_state(state)
        .layer(axum::middleware::map_response(no_store))
}

/// What the four admin handlers read.
struct AdminState {
    /// The readiness facts.
    runtime: Arc<RuntimeState>,
    /// The Prometheus text exposition renderer.
    render_metrics: Box<dyn Fn() -> String + Send + Sync>,
}

/// *This process's async runtime is scheduling tasks and the HTTP server can answer.*
///
/// It consults nothing external, ever, and it answers 200 from the moment the admin listener
/// binds until the process exits, INCLUDING throughout the drain. The only way it fails is by not
/// answering, which is exactly the condition a restart fixes. A liveness probe wired to a
/// dependency converts one database blip into a rolling restart of the whole fleet.
async fn live() -> Json<Liveness> {
    Json(Liveness { state: "live" })
}

/// *Route new work to me.*
async fn ready(State(state): State<Arc<AdminState>>) -> (StatusCode, Json<Readiness>) {
    let ready = state.runtime.is_ready();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Readiness {
            state: if ready { "ready" } else { "not_ready" },
            checks: state.runtime.checks(),
        }),
    )
}

/// Prometheus pull. One axum route calling the renderer: no second HTTP server, no push gateway.
async fn metrics(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        (state.render_metrics)(),
    )
}

/// The build identity, kept on the operator plane so a build fingerprint is not public.
#[allow(
    clippy::struct_field_names,
    reason = "the member names are the operator-facing JSON shape, not a naming choice"
)]
async fn version() -> Json<Version> {
    Json(Version {
        service: identity::SERVICE_NAME,
        version: identity::VERSION,
        git_sha: identity::GIT_SHA,
        rust_version: identity::RUST_VERSION,
    })
}

/// `Cache-Control: no-store` on every admin response, including the bare 404.
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /health/live`.
#[derive(Debug, serde::Serialize)]
struct Liveness {
    /// Always `live`. The property is `state`, not `status`: `status` is a banned property name.
    state: &'static str,
}

/// `GET /health/ready`.
#[derive(Debug, serde::Serialize)]
struct Readiness {
    /// `ready` | `not_ready`.
    state: &'static str,
    /// Name-sorted, never a map, so two consecutive bodies are byte-identical.
    checks: Vec<Check>,
}

/// `GET /version`.
// The member names are the operator-facing JSON keys, not a naming choice; renaming them to
// satisfy the lint would rename the response body.
#[expect(
    clippy::struct_field_names,
    reason = "`version` and `rust_version` are the documented JSON field names of this response"
)]
#[derive(Debug, serde::Serialize)]
struct Version {
    /// The one wire identity of this bounded context.
    service: &'static str,
    /// The crate version.
    version: &'static str,
    /// The build's git commit, or `unknown` outside a container build.
    git_sha: &'static str,
    /// The declared toolchain.
    rust_version: &'static str,
}
