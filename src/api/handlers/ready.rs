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

//! Readiness endpoint `/-/ready` (issue #324).
//!
//! `/metrics` answers `200` from the moment axum binds the listener, and
//! after #324 it always carries a baseline (`all_smi_up`,
//! `all_smi_build_info`) so a scrape in the pre-first-collection window is
//! no longer an indistinguishable zero bytes. That keeps every existing
//! scraper working, but it deliberately does *not* give an orchestrator a
//! yes/no signal it can gate on: a Kubernetes `readinessProbe` pointed at
//! `/metrics` passes immediately, before there is anything to serve.
//!
//! This module is that yes/no signal. `/-/ready` returns `503` until the
//! first collection cycle has populated `AppState`, and `200` afterwards.
//!
//! The path follows the Prometheus-ecosystem convention (`/-/ready` on
//! Prometheus itself, Alertmanager, and the Pushgateway) rather than
//! inventing `/ready` or `/healthz`. The `/-/` prefix is the ecosystem's
//! way of keeping operational endpoints out of the namespace a scrape
//! target might otherwise want, so an operator who already knows one
//! Prometheus component knows this one.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::app_state::AppState;

use super::SharedState;

/// Body returned once the first collection cycle has completed.
const READY_BODY: &str = "all-smi is ready.\n";

/// Body returned during the pre-first-collection window.
const NOT_READY_BODY: &str = "all-smi is not ready: no collection cycle has completed yet.\n";

/// Whether the exporter has completed at least one collection cycle.
///
/// This is the single source of truth behind *both* halves of the #324
/// contract: the `all_smi_up` gauge in the exposition and the `/-/ready`
/// status code. Keeping one predicate is the point. If the two were
/// computed independently they could disagree, and a consumer that gates
/// on `/-/ready` but alerts on `all_smi_up` would see a contradiction it
/// has no way to resolve.
///
/// [`AppState::loading`] starts `true` and is cleared by
/// [`crate::api::collection_loop::run_collection_loop`] at the end of its
/// first iteration, which is exactly the transition being described. It is
/// never set back to `true` on the API path.
pub fn is_ready(state: &AppState) -> bool {
    !state.loading
}

pub async fn ready_handler(State(state): State<SharedState>) -> Response {
    let ready = is_ready(&*state.read().await);
    readiness_response(ready)
}

/// Build the response for a given readiness verdict. Split out from the
/// handler so the status/header/body contract is unit-testable without an
/// axum `State` extractor or a live `AppState`.
fn readiness_response(ready: bool) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    // A readiness verdict is the last thing that should be cached. Without
    // this, a reverse proxy in front of the exporter can keep serving the
    // startup `503` long after the process became ready, which turns a
    // two-second window into an outage. The `/snapshot` handler takes the
    // same posture for the same reason.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    if ready {
        (StatusCode::OK, headers, READY_BODY).into_response()
    } else {
        // RFC 9110 says a 503 SHOULD carry Retry-After when the server
        // knows roughly how long the condition lasts, and here it does:
        // readiness arrives within one collection interval. One second is
        // the honest floor for "poll again shortly" and matches what the
        // CI gates do anyway.
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        (StatusCode::SERVICE_UNAVAILABLE, headers, NOT_READY_BODY).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_state_is_not_ready() {
        // `AppState::default()` is the state the server serves from
        // between bind and the first collection cycle.
        let state = AppState::default();
        assert!(state.loading, "precondition: a fresh AppState is loading");
        assert!(!is_ready(&state));
    }

    #[test]
    fn cleared_loading_flag_is_ready() {
        // Mirrors what `run_collection_loop` does at the end of its first
        // iteration.
        let state = AppState {
            loading: false,
            ..Default::default()
        };
        assert!(is_ready(&state));
    }

    #[test]
    fn not_ready_is_503_with_retry_after_and_no_store() {
        let response = readiness_response(false);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let headers = response.headers();
        assert_eq!(headers.get(header::RETRY_AFTER).unwrap(), "1");
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn ready_is_200_without_retry_after() {
        let response = readiness_response(true);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::RETRY_AFTER).is_none(),
            "Retry-After is meaningless on a 200 and would confuse a proxy"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
