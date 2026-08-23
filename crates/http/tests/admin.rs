//! The admin plane: liveness that never lies, readiness that names its failures, metrics,
//! version. Spec: `operator-health`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use axum::Router;
use http::StatusCode;
use ratatoskr_vault_http::{RuntimeState, admin_router};
use std::sync::Arc;
use tower::Service as _;

/// The fake Prometheus text a test renders; what the route must pass through untouched.
const RENDERED_METRICS: &str = "# HELP vault_build_info Build identity.\nvault_build_info 1\n";

fn router_with(state: RuntimeState) -> Router {
    admin_router(Arc::new(state), || RENDERED_METRICS.to_owned())
}

/// The same router over a shared state, for tests that flip readiness after building it.
fn router_over(state: Arc<RuntimeState>) -> Router {
    admin_router(state, || RENDERED_METRICS.to_owned())
}

async fn get(router: &mut Router, path: &str) -> (StatusCode, String, http::HeaderMap) {
    let response = tower::ServiceExt::<http::Request<axum::body::Body>>::ready(router)
        .await
        .expect("the router must be ready")
        .call(
            http::Request::builder()
                .method(http::Method::GET)
                .uri(path)
                .body(axum::body::Body::empty())
                .expect("a valid request"),
        )
        .await
        .expect("the router must answer");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a readable body");
    (status, String::from_utf8_lossy(&body).into_owned(), headers)
}

/// Liveness answers 200 from bind until exit, including throughout the drain.
#[tokio::test]
async fn liveness_answers_200_throughout_the_drain() {
    let state = RuntimeState::new();
    state.mark_startup_complete();
    state.begin_draining();

    let mut router = router_with(state);
    let (status, _, _) = get(&mut router, "/health/live").await;
    assert_eq!(status, StatusCode::OK);
}

/// Readiness is 503 until startup completes, then 200 with named checks.
#[tokio::test]
async fn readiness_flips_from_503_to_200_at_startup_completion() {
    let state = Arc::new(RuntimeState::new());
    let mut router = router_over(Arc::clone(&state));

    let (status, body, _) = get(&mut router, "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.contains("\"state\":\"not_ready\"") && body.contains("startup"),
        "an unstarted process names the failing check: {body}"
    );

    state.mark_startup_complete();
    let (status, body, _) = get(&mut router, "/health/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("\"state\":\"ready\""),
        "a started process reports ready: {body}"
    );
}

/// A configured-but-down database is a named failed check, not silence.
#[tokio::test]
async fn a_down_database_is_a_failed_named_check() {
    let state = RuntimeState::new();
    state.set_database_reachable(false);

    let mut router = router_with(state);
    let (status, body, _) = get(&mut router, "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.contains("\"database\""),
        "the database check must be present: {body}"
    );
    assert!(
        body.contains("dependency_unavailable"),
        "the closed-vocabulary reason must name the dependency: {body}"
    );
}

/// A database that was never configured produces NO database check at all: a passing check for
/// something that does not exist is the readiness equivalent of an always-zero metric.
#[tokio::test]
async fn an_absent_database_produces_no_database_check() {
    let state = RuntimeState::new();
    state.mark_startup_complete();

    let mut router = router_with(state);
    let (_, body, _) = get(&mut router, "/health/ready").await;
    assert!(
        !body.contains("database"),
        "no database is configured, so no database check may appear: {body}"
    );
}

/// Two consecutive bodies are byte-identical, so diff is a usable tool at 03:00.
#[tokio::test]
async fn consecutive_readiness_bodies_are_byte_identical() {
    let state = RuntimeState::new();
    state.mark_startup_complete();

    let mut router = router_with(state);
    let (_, first, _) = get(&mut router, "/health/ready").await;
    let (_, second, _) = get(&mut router, "/health/ready").await;
    assert_eq!(first, second);
}

/// Metrics is one route rendering the process registry as Prometheus text.
#[tokio::test]
async fn metrics_renders_prometheus_text_with_the_content_type() {
    let mut router = router_with(RuntimeState::new());
    let (status, body, headers) = get(&mut router, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, RENDERED_METRICS, "the renderer's text is the body");
    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4")
    );
}

/// Version carries the four fields an operator needs to know what is running.
#[tokio::test]
async fn version_carries_the_build_identity() {
    let mut router = router_with(RuntimeState::new());
    let (status, body, _) = get(&mut router, "/version").await;
    assert_eq!(status, StatusCode::OK);
    for field in ["service", "version", "git_sha", "rust_version"] {
        assert!(body.contains(field), "/version must carry {field}: {body}");
    }
    assert!(
        body.contains("ratatoskr-vault"),
        "the service name is the wire identity: {body}"
    );
}

/// Every admin response sets no-store, including the bare 404: a cached ready is a routing
/// decision made from stale data.
#[tokio::test]
async fn every_response_including_the_unknown_path_sets_no_store() {
    let mut router = router_with(RuntimeState::new());
    for path in ["/health/ready", "/definitely-not-a-route"] {
        let (_, _, headers) = get(&mut router, path).await;
        assert_eq!(
            headers
                .get(http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "{path} must set Cache-Control: no-store"
        );
    }
}
