// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
//! Integration tests for the pre-first-collection contract (issue #324).
//!
//! The acceptance criterion these exist for is specifically that the
//! behaviour is pinned *behind the live route with an empty `AppState`*,
//! not just at the renderer in isolation. The renderer-level tests live in
//! `src/api/metrics/render.rs`; a passing renderer test would not have
//! caught a handler that forgot to pass `ready`, nor a router that never
//! mounted `/-/ready`.
//!
//! No hardware readers are involved: the collection loop is simulated by
//! flipping `AppState::loading`, which is exactly the transition
//! `run_collection_loop` performs at the end of its first iteration.

#![cfg(feature = "cli")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use all_smi::api::FrameBus;
use all_smi::api::handlers::{metrics_handler, ready_handler};
use all_smi::api::server_state::ApiState;
use all_smi::app_state::AppState;
use axum::{Router, routing::get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::timeout;

/// Hard ceiling on any single request in this suite. Everything here is
/// in-process against a loopback listener, so a request that takes longer
/// than this is hung, not slow.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

type Shared = Arc<RwLock<AppState>>;

/// Build the router with the same two routes `server.rs` mounts for this
/// contract, and hand back the shared state so a test can simulate the
/// first collection cycle completing.
fn build_router() -> (Router, Shared) {
    let shared: Shared = Arc::new(RwLock::new(AppState::default()));
    let bus = FrameBus::new(Duration::from_secs(3));
    let state = ApiState::new(shared.clone(), bus);
    let router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/-/ready", get(ready_handler))
        .with_state(state);
    (router, shared)
}

async fn spawn_server(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Issue one HTTP/1.1 GET and return the whole raw response. `Connection:
/// close` means the read terminates at EOF, so there is no framing logic
/// here and no way for the read to hang past `REQUEST_TIMEOUT`.
async fn http_get(addr: SocketAddr, path: &str) -> String {
    let fut = async {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut buf = String::new();
        stream
            .read_to_string(&mut buf)
            .await
            .expect("read response");
        buf
    };
    timeout(REQUEST_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("GET {path} did not complete within {REQUEST_TIMEOUT:?}"))
}

/// Split a raw HTTP response into (status line + headers, body).
fn split_response(raw: &str) -> (&str, &str) {
    raw.split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response:\n{raw}"))
}

/// Simulate the collection loop finishing its first iteration.
async fn complete_first_collection(shared: &Shared) {
    shared.write().await.loading = false;
}

// ---------------------------------------------------------------------
// /metrics: 200 throughout, never byte-empty.
// ---------------------------------------------------------------------

#[tokio::test]
async fn metrics_is_200_and_non_empty_before_the_first_collection() {
    let (router, _shared) = build_router();
    let addr = spawn_server(router).await;

    let raw = http_get(addr, "/metrics").await;
    let (head, body) = split_response(&raw);

    assert!(
        head.starts_with("HTTP/1.1 200 OK"),
        "/metrics must stay 200 in the pre-first-collection window, got:\n{head}"
    );
    assert!(
        !body.is_empty(),
        "/metrics must never answer 200 with a byte-empty body (issue #324)"
    );
    assert!(
        body.lines().any(|l| l.starts_with("all_smi_up{")),
        "missing all_smi_up baseline:\n{body}"
    );
    assert!(
        body.lines().any(|l| l.starts_with("all_smi_build_info{")),
        "missing all_smi_build_info baseline:\n{body}"
    );
}

#[tokio::test]
async fn metrics_reports_up_zero_then_one_across_the_transition() {
    let (router, shared) = build_router();
    let addr = spawn_server(router).await;

    let before = http_get(addr, "/metrics").await;
    let (_, body) = split_response(&before);
    let up = body
        .lines()
        .find(|l| l.starts_with("all_smi_up{"))
        .expect("all_smi_up before collection");
    assert!(
        up.ends_with(" 0"),
        "expected all_smi_up 0 before the first cycle, got: {up}"
    );

    complete_first_collection(&shared).await;

    let after = http_get(addr, "/metrics").await;
    let (_, body) = split_response(&after);
    let up = body
        .lines()
        .find(|l| l.starts_with("all_smi_up{"))
        .expect("all_smi_up after collection");
    assert!(
        up.ends_with(" 1"),
        "expected all_smi_up 1 after the first cycle, got: {up}"
    );
}

/// The exact regression that motivated the CI workaround in PR #323: a
/// gate matching `^all_smi_` used to race the collection loop. With the
/// baseline in place the pattern matches from the very first request.
#[tokio::test]
async fn an_all_smi_prefixed_line_exists_from_the_first_request() {
    let (router, _shared) = build_router();
    let addr = spawn_server(router).await;

    let raw = http_get(addr, "/metrics").await;
    let (_, body) = split_response(&raw);
    assert!(
        body.lines().any(|l| l.starts_with("all_smi_")),
        "no ^all_smi_ line in the pre-first-collection body:\n{body}"
    );
}

// ---------------------------------------------------------------------
// /-/ready: 503 before, 200 after.
// ---------------------------------------------------------------------

#[tokio::test]
async fn ready_is_503_before_the_first_collection() {
    let (router, _shared) = build_router();
    let addr = spawn_server(router).await;

    let raw = http_get(addr, "/-/ready").await;
    let (head, body) = split_response(&raw);

    assert!(
        head.starts_with("HTTP/1.1 503 Service Unavailable"),
        "/-/ready must be 503 before the first collection cycle, got:\n{head}"
    );
    assert!(
        head.to_ascii_lowercase().contains("retry-after: 1"),
        "503 should advertise Retry-After, got:\n{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "a readiness verdict must not be cacheable, got:\n{head}"
    );
    assert!(body.contains("not ready"), "unhelpful 503 body: {body:?}");
}

#[tokio::test]
async fn ready_is_200_after_the_first_collection() {
    let (router, shared) = build_router();
    let addr = spawn_server(router).await;

    complete_first_collection(&shared).await;

    let raw = http_get(addr, "/-/ready").await;
    let (head, body) = split_response(&raw);

    assert!(
        head.starts_with("HTTP/1.1 200 OK"),
        "/-/ready must be 200 once a cycle has completed, got:\n{head}"
    );
    assert!(body.contains("ready"), "unhelpful 200 body: {body:?}");
}

/// `/-/ready` and `all_smi_up` are two views of one predicate. A consumer
/// that gates on one and alerts on the other must never see them
/// disagree, so assert them against each other on both sides of the
/// transition rather than only in isolation.
#[tokio::test]
async fn ready_endpoint_and_up_gauge_never_disagree() {
    let (router, shared) = build_router();
    let addr = spawn_server(router).await;

    for expect_ready in [false, true] {
        if expect_ready {
            complete_first_collection(&shared).await;
        }

        let ready_raw = http_get(addr, "/-/ready").await;
        let (ready_head, _) = split_response(&ready_raw);
        let endpoint_says_ready = ready_head.starts_with("HTTP/1.1 200 OK");

        let metrics_raw = http_get(addr, "/metrics").await;
        let (_, metrics_body) = split_response(&metrics_raw);
        let gauge_says_ready = metrics_body
            .lines()
            .find(|l| l.starts_with("all_smi_up{"))
            .expect("all_smi_up sample")
            .ends_with(" 1");

        assert_eq!(
            endpoint_says_ready, gauge_says_ready,
            "/-/ready and all_smi_up disagreed (expected ready={expect_ready})"
        );
        assert_eq!(endpoint_says_ready, expect_ready);
    }
}

/// An unmounted neighbour of the readiness path must still 404. This
/// guards against someone "fixing" the unusual `/-/` prefix by mounting a
/// wildcard, which would silently answer 200 for typos.
#[tokio::test]
async fn unrelated_paths_are_not_captured_by_the_readiness_route() {
    let (router, _shared) = build_router();
    let addr = spawn_server(router).await;

    let raw = http_get(addr, "/-/healthy").await;
    let (head, _) = split_response(&raw);
    assert!(
        head.starts_with("HTTP/1.1 404"),
        "only /-/ready is mounted; got:\n{head}"
    );
}
