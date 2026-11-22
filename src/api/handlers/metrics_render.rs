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

//! Prometheus `/metrics` HTTP handler.
//!
//! Serves the exposition the collection loop rendered for the current
//! tick (issue #503). Every input of the exposition changes only inside
//! the loop, so rendering per scrape repeated identical work for every
//! Prometheus server, dashboard and ad hoc `curl`, and held the `AppState`
//! read lock while doing it. The loop now renders once per tick through
//! the shared writer in [`crate::api::metrics::render`] and stores the
//! body in [`AppState::metrics_body`]; this handler only clones it. Kept
//! separate from the SSE / snapshot handlers (issue #193) so adding new
//! routes does not force a rebuild of this unchanged hot path.
//!
//! Status-code contract (issue #324): this endpoint answers `200` for its
//! entire lifetime, including the window between the listener binding and
//! the first collection cycle landing. It is deliberately *not* a
//! readiness probe, because turning it into one would break every scraper
//! that already treats a non-200 as a failed target. What changed in #324
//! is that the body is no longer empty in that window: the exposition
//! carries `all_smi_up 0` plus build info until the first cycle completes,
//! then `all_smi_up 1` plus the full metric set. Consumers that need a
//! yes/no gate use `/-/ready` (see [`crate::api::handlers::ready`]).
//! [`AppState`] is constructed with the not-ready render already stored,
//! so that window needs no special case here.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::api::handlers::ready::is_ready;
use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::app_state::AppState;

pub type SharedState = Arc<RwLock<AppState>>;

/// Content type of the exposition. Unchanged from when the handler
/// returned a `String`.
const METRICS_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

pub async fn metrics_handler(State(state): State<SharedState>) -> Response {
    // `Bytes` is reference counted: this is a pointer clone under a read
    // lock that is released before the response is written.
    let body = state.read().await.metrics_body.clone();
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(METRICS_CONTENT_TYPE),
        )],
        body,
    )
        .into_response()
}

/// Render the exposition from the device vectors and flags held in
/// `state` itself.
///
/// The live collection loop renders from its own locals instead (see
/// [`crate::api::collection_loop`]); this is for the points where no tick
/// is involved: the initial not-ready body, and a caller that mutates an
/// `AppState` directly and needs `/metrics` to reflect it.
pub fn render_metrics_body(state: &AppState) -> Bytes {
    let inputs = MetricsRenderInputs {
        gpu_info: &state.gpu_info,
        process_info: &state.process_info,
        cpu_info: &state.cpu_info,
        memory_info: &state.memory_info,
        storage_info: &state.storage_info,
        runtime_environment: &state.runtime_environment,
        chassis_info: &state.chassis_info,
        vgpu_info: &state.vgpu_info,
        mig_info: &state.mig_info,
        // Energy counter (issue #191) reflects the integrator owned by
        // AppState; we export the PowerIntegrator directly so the
        // counter's HELP/TYPE header lines are only emitted when there
        // is at least one device with recorded samples.
        energy_integrator: Some(state.energy.integrator()),
        // Same predicate `/-/ready` answers with (issue #324).
        ready: is_ready(state),
    };
    Bytes::from(render_prometheus_exposition(&inputs))
}

/// Re-render [`AppState::metrics_body`] from the state's own fields.
pub fn refresh_metrics_body(state: &mut AppState) {
    state.metrics_body = render_metrics_body(state);
}
