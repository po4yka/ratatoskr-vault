//! Loopback-only in-memory S3-compatible fixture server.

#![allow(clippy::expect_used, reason = "fixture setup failures must fail tests")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::{Method, Request, Response, StatusCode};
use axum::routing::any;

#[derive(Debug, Default)]
struct FixtureState {
    objects: Mutex<BTreeMap<String, Bytes>>,
    uploads: Mutex<BTreeMap<String, FixtureUpload>>,
    requests: Mutex<Vec<String>>,
    corrupt_gets: AtomicBool,
    retain_deletes: AtomicBool,
    sequence: AtomicU64,
}

#[derive(Debug)]
struct FixtureUpload {
    key: String,
    parts: BTreeMap<u32, Bytes>,
}

/// An Axum S3 subset bound to an ephemeral loopback port.
#[derive(Debug)]
pub(crate) struct S3Fixture {
    endpoint: String,
    state: Arc<FixtureState>,
    task: tokio::task::JoinHandle<()>,
}

impl S3Fixture {
    /// Starts the fixture without public network or fixed-port dependencies.
    pub(crate) async fn start() -> Self {
        let state = Arc::new(FixtureState::default());
        let app = Router::new()
            .route("/{*key}", any(handle))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener must bind");
        let address = listener
            .local_addr()
            .expect("fixture listener must have an address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fixture server must remain available");
        });
        Self {
            endpoint: format!("http://{address}"),
            state,
            task,
        }
    }

    /// Loopback endpoint accepted by replica configuration validation.
    #[must_use]
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Bounded diagnostic request sequence for failed fixture assertions.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.state
            .requests
            .lock()
            .map_or_else(|_| Vec::new(), |requests| requests.clone())
    }

    /// Corrupts the next successful object GET without changing stored fixture bytes.
    pub(crate) fn corrupt_next_get(&self) {
        self.state.corrupt_gets.store(true, Ordering::Release);
    }

    /// Acknowledges the next object DELETE while deliberately retaining its bytes.
    pub(crate) fn retain_next_delete(&self) {
        self.state.retain_deletes.store(true, Ordering::Release);
    }
}

impl Drop for S3Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<Arc<FixtureState>>, request: Request<Body>) -> Response<Body> {
    let key = request.uri().path().trim_start_matches('/').to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    if let Ok(mut requests) = state.requests.lock() {
        requests.push(format!("{} /{key}?{query}", request.method()));
    }
    match *request.method() {
        Method::POST if query == "delete" || query.starts_with("delete=") => {
            delete_objects(&state, request.into_body()).await
        }
        Method::POST if query_value(&query, "uploads").is_some() => initiate_upload(&state, &key),
        Method::PUT if query_value(&query, "uploadId").is_some() => {
            upload_part(&state, key, query, request.into_body()).await
        }
        Method::POST if query_value(&query, "uploadId").is_some() => {
            complete_upload(&state, &query)
        }
        Method::DELETE if query_value(&query, "uploadId").is_some() => abort_upload(&state, &query),
        Method::DELETE => match state.objects.lock() {
            Ok(mut objects) => {
                if !state.retain_deletes.swap(false, Ordering::AcqRel) {
                    objects.remove(&key);
                }
                response(StatusCode::NO_CONTENT, Body::empty())
            }
            Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
        },
        Method::PUT => {
            let Ok(body) = to_bytes(request.into_body(), 64 * 1024 * 1024).await else {
                return response(StatusCode::BAD_REQUEST, Body::empty());
            };
            match state.objects.lock() {
                Ok(mut objects) => {
                    objects.insert(key, body);
                    response(StatusCode::OK, Body::empty())
                }
                Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
            }
        }
        Method::GET => match state.objects.lock() {
            Ok(objects) => objects.get(&key).map_or_else(
                || response(StatusCode::NOT_FOUND, Body::empty()),
                |bytes| {
                    let mut returned = bytes.to_vec();
                    if state.corrupt_gets.swap(false, Ordering::AcqRel)
                        && let Some(first) = returned.first_mut()
                    {
                        *first ^= 0xff;
                    }
                    object_response(StatusCode::OK, Bytes::from(returned))
                },
            ),
            Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
        },
        Method::HEAD => match state.objects.lock() {
            Ok(objects) if objects.contains_key(&key) => response(StatusCode::OK, Body::empty()),
            Ok(_) => response(StatusCode::NOT_FOUND, Body::empty()),
            Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
        },
        _ => response(StatusCode::METHOD_NOT_ALLOWED, Body::empty()),
    }
}

async fn delete_objects(state: &FixtureState, body: Body) -> Response<Body> {
    let Ok(bytes) = to_bytes(body, 1024 * 1024).await else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    let Ok(xml) = std::str::from_utf8(&bytes) else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    let Some(key) = xml
        .split_once("<Key>")
        .and_then(|(_, remainder)| remainder.split_once("</Key>"))
        .map(|(key, _)| key)
    else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    match state.objects.lock() {
        Ok(mut objects) => {
            if !state.retain_deletes.swap(false, Ordering::AcqRel) {
                objects.remove(key);
                objects.remove(&format!("vault-fixtures/{key}"));
            }
            xml_response(
                StatusCode::OK,
                format!(
                    "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Deleted><Key>{key}</Key></Deleted></DeleteResult>"
                ),
            )
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    }
}

fn initiate_upload(state: &FixtureState, key: &str) -> Response<Body> {
    let upload_id = state.sequence.fetch_add(1, Ordering::Relaxed).to_string();
    match state.uploads.lock() {
        Ok(mut uploads) => {
            uploads.insert(
                upload_id.clone(),
                FixtureUpload {
                    key: key.to_owned(),
                    parts: BTreeMap::new(),
                },
            );
            let body = format!(
                "<InitiateMultipartUploadResult><Bucket>vault-fixtures</Bucket><Key>{key}</Key><UploadId>{upload_id}</UploadId></InitiateMultipartUploadResult>"
            );
            xml_response(StatusCode::OK, body)
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    }
}

async fn upload_part(
    state: &FixtureState,
    key: String,
    query: String,
    body: Body,
) -> Response<Body> {
    let Some(upload_id) = query_value(&query, "uploadId") else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    let Some(part_number) =
        query_value(&query, "partNumber").and_then(|value| value.parse::<u32>().ok())
    else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    let Ok(bytes) = to_bytes(body, 64 * 1024 * 1024).await else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    match state.uploads.lock() {
        Ok(mut uploads) => match uploads.get_mut(upload_id) {
            Some(upload) if upload.key == key => {
                upload.parts.insert(part_number, bytes);
                Response::builder()
                    .status(StatusCode::OK)
                    .header("etag", format!("\"fixture-{part_number}\""))
                    .body(Body::empty())
                    .expect("fixture part response must build")
            }
            _ => response(StatusCode::NOT_FOUND, Body::empty()),
        },
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    }
}

fn complete_upload(state: &FixtureState, query: &str) -> Response<Body> {
    let Some(upload_id) = query_value(query, "uploadId") else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    let upload = match state.uploads.lock() {
        Ok(mut uploads) => uploads.remove(upload_id),
        Err(_) => return response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    };
    let Some(upload) = upload else {
        return response(StatusCode::NOT_FOUND, Body::empty());
    };
    let mut bytes = Vec::new();
    for part in upload.parts.into_values() {
        bytes.extend_from_slice(&part);
    }
    match state.objects.lock() {
        Ok(mut objects) => {
            objects.insert(upload.key.clone(), Bytes::from(bytes));
            xml_response(
                StatusCode::OK,
                format!(
                    "<CompleteMultipartUploadResult><Location>fixture</Location><Bucket>vault-fixtures</Bucket><Key>{}</Key><ETag>\"fixture-complete\"</ETag></CompleteMultipartUploadResult>",
                    upload.key
                ),
            )
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    }
}

fn abort_upload(state: &FixtureState, query: &str) -> Response<Body> {
    let Some(upload_id) = query_value(query, "uploadId") else {
        return response(StatusCode::BAD_REQUEST, Body::empty());
    };
    match state.uploads.lock() {
        Ok(mut uploads) => {
            uploads.remove(upload_id);
            response(StatusCode::NO_CONTENT, Body::empty())
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty()),
    }
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn object_response(status: StatusCode, bytes: Bytes) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-length", bytes.len())
        .header("etag", "\"fixture-object\"")
        .header("last-modified", "Wed, 21 Oct 2015 07:28:00 GMT")
        .body(Body::from(bytes))
        .expect("fixture object response must build")
}

fn xml_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/xml")
        .body(Body::from(body))
        .expect("fixture XML response must build")
}

fn response(status: StatusCode, body: Body) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body)
        .expect("fixture response must build")
}
